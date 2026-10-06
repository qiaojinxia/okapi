import { RotateCcw } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { cn } from '@/lib/utils'
import type { UsageResponse } from './api'
import { useQuotaLabels } from './use-quota-labels'

type Quota = NonNullable<UsageResponse['quotas'][number]['quota']>

/// 用量越接近上限越醒目：70% 起转黄，90% 起转红（与渠道额度阈值缺省 90% 对齐）。
function level(percent: number) {
  if (percent >= 90) return { bar: 'bg-destructive', text: 'text-destructive' }
  if (percent >= 70) return { bar: 'bg-warning', text: 'text-warning' }
  return { bar: 'bg-success', text: 'text-foreground' }
}

/// 订阅额度各窗口的用量条。`limits` 是渠道配置的百分比上限（窗口秒数 → 百分比），
/// 画成一条竖线，站长能一眼看出离自动停用还有多远。`compact` 用于列表行。
export function QuotaMeters({ quota, limits, compact = false }: {
  quota: Quota
  limits?: Record<string, number | null | undefined>
  compact?: boolean
}) {
  const { t, i18n } = useTranslation()
  const { windowLabel } = useQuotaLabels()
  const now = Date.now()
  // 列表行的重置说明：「3小时50分后重置 · 06:59」，跨天带日期
  const resetCompact = (at: number) => {
    const minutes = Math.max(0, Math.round((at * 1000 - now) / 60_000))
    const d = Math.floor(minutes / 1440), h = Math.floor((minutes % 1440) / 60), m = minutes % 60
    const span = d > 0 ? t('admin:channelQuotaSpanDH', { d, h }) : h > 0 ? t('admin:channelQuotaSpanHM', { h, m }) : t('admin:channelQuotaSpanM', { m })
    const date = new Date(at * 1000)
    const sameDay = date.toDateString() === new Date(now).toDateString()
    const clock = date.toLocaleString(i18n.language, sameDay
      ? { hour: '2-digit', minute: '2-digit', hour12: false }
      : { month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit', hour12: false })
    return t('admin:channelQuotaResetCompact', { span, at: clock })
  }
  const resetIn = (at: number) => {
    const minutes = Math.max(0, Math.round((at * 1000 - now) / 60_000))
    const format = new Intl.RelativeTimeFormat(i18n.language, { numeric: 'auto', style: 'narrow' })
    return minutes >= 1440 ? format.format(Math.round(minutes / 1440), 'day')
      : minutes >= 60 ? format.format(Math.round(minutes / 60), 'hour')
        : format.format(minutes, 'minute')
  }
  if (compact) {
    return (
      <div className="grid w-max grid-cols-[auto_4.5rem_2.5rem] min-[1400px]:grid-cols-[auto_4.5rem_2.5rem_auto] items-center gap-x-2 gap-y-1 text-xs">
        {quota.allowed === false && <p className="col-span-4 text-destructive">{t('admin:channelQuotaBlocked')}</p>}
        {quota.windows.map((window) => {
          const stale = window.resets_at !== null && window.resets_at * 1000 <= now
          const percent = Math.min(100, Math.max(0, window.used_percent))
          const tone = level(percent)
          const label = windowLabel(window.window_secs)
          return (
            <div key={window.name} className="contents">
              <span className="whitespace-nowrap text-muted-foreground">{label}</span>
              <div className="h-1.5 overflow-hidden rounded-full bg-muted" role="progressbar"
                aria-label={t('admin:channelQuotaMeterLabel', { window: label })}
                aria-valuemin={0} aria-valuemax={100} aria-valuenow={stale ? undefined : percent}>
                {!stale && <div className={cn('h-full rounded-full', tone.bar)} style={{ width: `${percent}%` }} />}
              </div>
              <span className={cn('text-right font-medium tabular-nums', stale ? 'text-muted-foreground' : tone.text)}>
                {stale ? '—' : `${percent}%`}
              </span>
              {/* 窄屏把重置时间放到条形下一行，避免表格横向滚动 */}
              <span className="col-span-3 col-start-2 -mt-0.5 inline-flex items-center gap-1 whitespace-nowrap text-[11px] text-muted-foreground tabular-nums min-[1400px]:col-span-1 min-[1400px]:col-start-auto min-[1400px]:mt-0 min-[1400px]:text-xs">
                {stale ? t('admin:channelQuotaRefreshing') : window.resets_at !== null && <>
                  <RotateCcw aria-label={t('admin:channelQuotaResetLabel')} className="h-3 w-3 shrink-0" />
                  {resetCompact(window.resets_at)}
                </>}
              </span>
            </div>
          )
        })}
      </div>
    )
  }
  return (
    <div className="flex flex-col gap-2.5">
      {quota.allowed === false && <p className="text-xs text-destructive">{t('admin:channelQuotaBlocked')}</p>}
      {quota.windows.map((window) => {
        const stale = window.resets_at !== null && window.resets_at * 1000 <= now
        const percent = Math.min(100, Math.max(0, window.used_percent))
        const limit = window.window_secs !== null ? limits?.[String(window.window_secs)] : undefined
        const tone = level(percent)
        const label = windowLabel(window.window_secs)
        return (
          <div key={window.name} className="flex flex-col gap-1">
            <div className="flex items-baseline justify-between gap-2 text-xs">
              <span className="text-muted-foreground">{label}</span>
              <span className={cn('font-medium tabular-nums', stale ? 'text-muted-foreground' : tone.text)}>
                {stale ? t('admin:channelQuotaRefreshing') : `${percent}%`}
              </span>
            </div>
            <div
              className="relative h-2 w-full overflow-hidden rounded-full bg-muted"
              role="progressbar"
              aria-label={t('admin:channelQuotaMeterLabel', { window: label })}
              aria-valuemin={0}
              aria-valuemax={100}
              aria-valuenow={stale ? undefined : percent}
            >
              {!stale && <div className={cn('h-full rounded-full transition-[width]', tone.bar)} style={{ width: `${percent}%` }} />}
              {typeof limit === 'number' && limit > 0 && limit < 100 && (
                <div className="absolute inset-y-0 w-0.5 bg-foreground/60" style={{ left: `${limit}%` }}
                  title={t('admin:channelQuotaLimitMarker', { percent: limit })} />
              )}
            </div>
            {window.resets_at !== null && !stale && (
              <span className="text-[11px] text-muted-foreground">
                {t('admin:channelQuotaResetsIn', { when: resetIn(window.resets_at) })}
                {typeof limit === 'number' && <> · {t('admin:channelQuotaLimitMarker', { percent: limit })}</>}
              </span>
            )}
          </div>
        )
      })}
    </div>
  )
}
