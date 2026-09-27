import { useQuery } from '@tanstack/react-query'
import { useId } from 'react'
import { useTranslation } from 'react-i18next'
import { Area, AreaChart, ResponsiveContainer, Tooltip, YAxis } from 'recharts'
import { Card, CardContent } from '@/components/ui/card'
import { ErrorState } from '@/components/ui/state'
import { InlineStat } from '@/components/ui/stat'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatBp, formatCount, formatMoneyAggregate } from '@/lib/money'
import { qk } from '@/lib/query-keys'
import { cn } from '@/lib/utils'
import { AttentionPreview, DashboardHealth } from './AttentionCard'
import { InventoryStrip } from './InventoryStrip'
import { Tooltip as HelpTooltip } from '@/components/ui/tooltip'

interface RealtimePoint { ts: number; requests: number; tokens: number; errors: number; amount_micro: number }
interface RealtimeResp {
  window_secs: number; qps_milli: number; requests: number; errors: number
  error_rate_bp: number; tokens: number; amount_micro: number; series: RealtimePoint[]
}

// Redis 秒桶与日聚合分开展示；请求失败时不能继续显示绿色“实时”状态。
export function RealtimeCard({ days }: { days: number }) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const gradientId = useId().replaceAll(':', '')
  const q = useQuery({
    queryKey: qk.statsRealtime,
    queryFn: () => apiFetch<RealtimeResp>('/admin/stats/realtime?window=60'),
    refetchInterval: 5_000,
    retry: false,
  })
  const d = q.isError ? undefined : q.data
  const qps = d ? (d.qps_milli / 1_000).toLocaleString(locale, { maximumFractionDigits: 1 }) : '—'
  const hasRequests = (d?.requests ?? 0) > 0
  const liveHint = [t('admin:dashboardLiveHint'), q.dataUpdatedAt > 0 && !q.isError ? t('common:updatedAt', { time: new Date(q.dataUpdatedAt).toLocaleTimeString(locale) }) : ''].filter(Boolean).join(' · ')
  return <Card className="min-w-0 rounded-xl" role="region" aria-label={t('admin:dashboardLive')}>
    <CardContent className="grid gap-3 px-4 py-3 lg:grid-cols-[12rem_minmax(0,1fr)] lg:items-center lg:py-1">
      <div className="flex flex-wrap items-center justify-between gap-2 lg:gap-0.5">
        <HelpTooltip content={liveHint}>
          <div tabIndex={0} className="flex flex-wrap items-center gap-x-2 gap-y-0.5 rounded outline-none focus-visible:ring-2 focus-visible:ring-primary/40">
            <span aria-hidden className={cn('h-2 w-2 rounded-full', d ? 'bg-success' : 'bg-muted-foreground/40')} />
            <span className="text-sm font-semibold">{t('admin:realtimeTitle')}</span>
            <span className="text-xs text-muted-foreground">{t('admin:dashboardLiveWindow')}</span>
          </div>
        </HelpTooltip>
        <DashboardHealth />
      </div>
      {q.isError ? <ErrorState message={t('admin:dashboardLiveUnavailable', { reason: describeError(q.error) })} onRetry={() => void q.refetch()} /> :
        <div className="grid min-w-0 gap-3 lg:grid-cols-[minmax(0,1fr)_5rem]">
          <div className="grid min-w-0 grid-cols-2 gap-x-4 gap-y-2 sm:grid-cols-3 lg:grid-cols-5">
            <InlineStat label="QPS" value={qps} />
            <InlineStat label={t('admin:realtimeReqs60')} value={d ? formatCount(d.requests, locale) : '—'} />
            <InlineStat label={t('admin:kpiErrorRate')} value={d && hasRequests ? formatBp(d.error_rate_bp, locale) : '—'} tone={hasRequests && d && d.error_rate_bp >= 500 ? 'bad' : hasRequests && d && d.error_rate_bp >= 100 ? 'warn' : 'default'} />
            <InlineStat label={t('admin:kpiTokens')} value={d ? formatCount(d.tokens, locale) : '—'} />
            <InlineStat label={t('admin:kpiRevenue')} value={d ? formatMoneyAggregate(d.amount_micro, locale) : '—'} />
          </div>
          <div className="hidden h-10 min-w-0 rounded-lg bg-muted/30 px-2 md:block" role="img" aria-label={t('admin:dashboardLiveChart')}>
            {d && d.series.some((p) => p.requests > 0) ? <ResponsiveContainer width="100%" height="100%" minWidth={0}>
              <AreaChart data={d.series} margin={{ top: 6, bottom: 3, left: 0, right: 0 }}>
                <defs><linearGradient id={gradientId} x1="0" y1="0" x2="0" y2="1"><stop offset="0%" stopColor="var(--color-primary)" stopOpacity={0.3} /><stop offset="100%" stopColor="var(--color-primary)" stopOpacity={0.02} /></linearGradient></defs>
                <YAxis hide domain={[0, 'auto']} />
                <Tooltip content={({ active, payload }) => active && payload?.length ? <div className="rounded-lg border border-border bg-popover px-2 py-1 text-xs text-popover-foreground shadow-popover">{t('common:requests')}: {Number(payload[0]?.value ?? 0).toLocaleString(locale)}</div> : null} />
                <Area type="linear" dataKey="requests" stroke="var(--color-primary)" fill={`url(#${gradientId})`} dot={d.series.length === 1 ? { r: 3 } : false} isAnimationActive={false} />
              </AreaChart>
            </ResponsiveContainer> : <div className="flex h-full items-center justify-center text-xs text-muted-foreground">{t(q.isPending ? 'common:loading' : 'admin:dashboardNoTraffic')}</div>}
          </div>
        </div>}
    </CardContent>
    <InventoryStrip compact />
    <AttentionPreview days={days} />
  </Card>
}
