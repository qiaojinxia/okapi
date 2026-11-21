import { useQuery } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { Tooltip } from '@/components/ui/tooltip'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatBp, formatCount, formatMoneyAggregate } from '@/lib/money'
import { qk } from '@/lib/query-keys'

interface RealtimeResp {
  window_secs: number; qps_milli: number; requests: number; errors: number
  error_rate_bp: number; tokens: number; amount_micro: number
}

/// 页头副行里的实时状态：最近 60 秒的流量用一句话说完，其余数字收进悬停说明。
/// Redis 秒桶与日聚合分开展示；请求失败时不能继续显示绿色“实时”状态。
export function LiveStatus() {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const q = useQuery({
    queryKey: qk.statsRealtime,
    queryFn: () => apiFetch<RealtimeResp>('/admin/stats/realtime?window=60'),
    refetchInterval: 5_000,
    retry: false,
  })
  const frame = (children: React.ReactNode) => <span role="region" aria-label={t('admin:dashboardLive')} className="inline-flex">{children}</span>
  if (q.isError) return frame(<span role="alert" className="inline-flex flex-wrap items-center gap-x-2 gap-y-0.5 text-warning">
    <span aria-hidden className="size-[7px] shrink-0 rounded-full bg-warning" />
    {t('admin:dashboardLiveUnavailable', { reason: describeError(q.error) })}
    <button type="button" onClick={() => void q.refetch()} className="rounded text-primary underline-offset-2 outline-none hover:underline focus-visible:ring-2 focus-visible:ring-primary/40">{t('common:retry')}</button>
  </span>)
  const d = q.data
  if (!d) return frame(<span role="status" className="inline-flex items-center gap-1.5">
    <span aria-hidden className="size-[7px] shrink-0 rounded-full bg-muted-foreground/40" />
    {t('admin:realtimeTitle')} · {t('common:loading')}
  </span>)
  const windowSecs = d.window_secs > 0 ? d.window_secs : 60
  const qps = (d.requests / windowSecs).toLocaleString(locale, { maximumFractionDigits: 3 })
  const recentQps = (d.qps_milli / 1_000).toLocaleString(locale, { maximumFractionDigits: 3 })
  const hasRequests = d.requests > 0
  const updated = q.dataUpdatedAt > 0 ? t('common:updatedAt', { time: new Date(q.dataUpdatedAt).toLocaleTimeString(locale) }) : ''
  const errorTone = d.error_rate_bp >= 500 ? 'text-destructive' : d.error_rate_bp > 0 ? 'text-warning' : 'text-foreground'
  const detailCard = (
    <div className="w-[256px] max-w-full space-y-2.5 text-left">
      <div className="flex items-center justify-between gap-2 border-b border-border/60 pb-2">
        <span className="inline-flex items-center gap-1.5 text-[11px] font-medium text-foreground">
          <span aria-hidden className="size-1.5 shrink-0 rounded-full bg-success" />
          {t('admin:dashboardLive')}
        </span>
        {updated && <span className="text-[11px] tabular-nums text-muted-foreground">{updated}</span>}
      </div>
      <div className="space-y-1.5 text-xs">
        <div className="flex items-center justify-between gap-4">
          <span className="text-muted-foreground">{t('admin:realtimeAvgQps')}</span>
          <span className="sr-only"> </span>
          <span className="font-semibold tabular-nums text-foreground">{qps}</span>
        </div>
        <div className="flex items-center justify-between gap-4">
          <span className="text-muted-foreground">{t('admin:realtimeReqs60')}</span>
          <span className="sr-only"> </span>
          <span className="font-semibold tabular-nums text-foreground">{formatCount(d.requests, locale)}</span>
        </div>
        {/* 最近一分钟没有调用时错误率没有意义，不显示成 0% */}
        {hasRequests && (
          <div className="flex items-center justify-between gap-4">
            <span className="text-muted-foreground">{t('admin:kpiErrorRate')}</span>
            <span className="sr-only"> </span>
            <span className={`font-semibold tabular-nums ${errorTone}`}>{formatBp(d.error_rate_bp, locale)}</span>
          </div>
        )}
        <div className="flex items-center justify-between gap-4">
          <span className="text-muted-foreground">{t('admin:kpiTokens')}</span>
          <span className="sr-only"> </span>
          <span className="font-semibold tabular-nums text-foreground">{formatCount(d.tokens, locale)}</span>
        </div>
        <div className="flex items-center justify-between gap-4">
          <span className="text-muted-foreground">{t('admin:kpiRevenue')}</span>
          <span className="sr-only"> </span>
          <span className="font-semibold tabular-nums text-foreground">{formatMoneyAggregate(d.amount_micro, locale, true)}</span>
        </div>
      </div>
      <div className="space-y-1 border-t border-border/60 pt-2 text-[11px] leading-relaxed text-muted-foreground">
        <p>{t('admin:realtimeQpsHint', { seconds: windowSecs, recentSeconds: Math.min(windowSecs, 10), recentQps })}</p>
        <p className="text-[10.5px] text-muted-foreground/80">{t('admin:dashboardLiveHint')}</p>
      </div>
    </div>
  )
  return frame(<Tooltip content={detailCard}>
    <span tabIndex={0} className="inline-flex items-center gap-1.5 rounded outline-none focus-visible:ring-2 focus-visible:ring-primary/40">
      <span aria-hidden className="size-[7px] shrink-0 rounded-full bg-success" />
      {hasRequests ? t('admin:dashboardLiveBusy', { n: formatCount(d.requests, locale), qps }) : t('admin:dashboardLiveIdle')}
    </span>
  </Tooltip>)
}
