import type { ReactNode } from 'react'
import { RotateCw } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { cn } from '@/lib/utils'
import type { UsageResponse } from './api'
import { useQuotaLabels } from './use-quota-labels'

type Quota = NonNullable<UsageResponse['quotas'][number]['quota']>
/// 画一条用量条所需的字段；`scope` 有值时是按模型分的周窗口（订阅的 Fable 周限额），标签换成模型名
type Row = { name: string; used_percent: number; resets_at: number | null; window_secs: number | null; scope?: string }

/// 红黄绿三种颜色表示还剩多少，列表行和抽屉里的条共用，不再按窗口另配颜色：
/// 用量不到 70%（剩得还多）绿，70% 起（快用完了）黄，90% 起（几乎没了，与渠道额度阈值缺省 90% 对齐）红。
export function quotaTone(percent: number) {
  if (percent >= 90) return { bar: 'bg-destructive', text: 'text-destructive' }
  if (percent >= 70) return { bar: 'bg-warning', text: 'text-warning' }
  return { bar: 'bg-success', text: 'text-foreground' }
}

/// 列表行里的一个小仪表，三层：「名字 + 百分比 / 细条 / 一行说明」。
/// 15 + 3 + 6 + 3 + 15 = 42px：正好是两行文字（20 + 2 + 20）的高度，有仪表的行不比别的行高。
/// `percent` 为 null 表示读数已过期（不画条，百分比处是破折号）；`marker` 在条上画一道上限刻度。
export function CompactMeter({ slot, label, title, meterLabel, percent, marker, markerTitle, footer, footerTitle }: {
  slot: string
  label: string
  title: string
  meterLabel: string
  percent: number | null
  marker?: number
  markerTitle?: string
  footer: ReactNode
  footerTitle?: string
}) {
  const tone = quotaTone(percent ?? 0)
  return (
    <div data-slot={slot} className="flex min-w-0 flex-col gap-[3px]">
      <div className="flex h-[15px] items-end justify-between gap-1.5 leading-[15px]">
        <span className="truncate text-[11px] text-muted-foreground" title={title}>{label}</span>
        <span className={cn('shrink-0 text-xs font-semibold tabular-nums', percent === null ? 'text-muted-foreground' : tone.text)}>
          {percent === null ? '—' : `${percent}%`}
        </span>
      </div>
      <div className="relative h-1.5 overflow-hidden rounded-full bg-muted" role="progressbar" aria-label={meterLabel}
        aria-valuemin={0} aria-valuemax={100} aria-valuenow={percent ?? undefined}>
        {percent !== null && <div className={cn('h-full rounded-full transition-[width]', tone.bar)} style={{ width: `${percent}%` }} />}
        {typeof marker === 'number' && marker > 0 && marker < 100 && (
          <div data-slot="quota-limit-marker" className="absolute inset-y-0 w-0.5 bg-foreground/60" style={{ left: `${marker}%` }} title={markerTitle} />
        )}
      </div>
      <span data-slot="quota-reset" className="flex h-[15px] min-w-0 items-center gap-1 text-[11px] leading-[15px] text-muted-foreground/80 tabular-nums" title={footerTitle}>
        {footer}
      </span>
    </div>
  )
}

/// 「3小时50分」这样的剩余时长，和它的完整说明（带具体时刻，跨天带日期）；列表行里的重置 / 恢复时间都用它。
export function useResetSpan() {
  const { t, i18n } = useTranslation()
  return (at: number, now = Date.now()) => {
    const minutes = Math.max(0, Math.round((at * 1000 - now) / 60_000))
    const d = Math.floor(minutes / 1440), h = Math.floor((minutes % 1440) / 60), m = minutes % 60
    const span = d > 0 ? t('admin:channelQuotaSpanDH', { d, h }) : h > 0 ? t('admin:channelQuotaSpanHM', { h, m }) : t('admin:channelQuotaSpanM', { m })
    const date = new Date(at * 1000)
    const sameDay = date.toDateString() === new Date(now).toDateString()
    const clock = date.toLocaleString(i18n.language, sameDay
      ? { hour: '2-digit', minute: '2-digit', hour12: false }
      : { month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit', hour12: false })
    return { span, full: t('admin:channelQuotaResetCompact', { span, at: clock }) }
  }
}

/// 订阅额度各窗口的用量条。`limits` 是渠道配置的百分比上限（窗口秒数 → 百分比），
/// 画成一条竖线，站长能一眼看出离自动停用还有多远。`compact` 用于列表行。
export function QuotaMeters({ quota, limits, compact = false }: {
  quota: Quota
  limits?: Record<string, number | null | undefined>
  compact?: boolean
}) {
  const { t, i18n } = useTranslation()
  const { windowLabel, shortWindowLabel } = useQuotaLabels()
  const resetSpan = useResetSpan()
  const now = Date.now()
  // 账号窗口之后再画按模型分的周窗口：同样的条。它们只展示——渠道配置的百分比上限（`limits`）管的是账号窗口，不适用于某个模型的周限额
  const rows: Row[] = [
    ...quota.windows,
    ...(quota.scoped ?? []).map((window): Row => ({ ...window, name: `scoped:${window.model}`, scope: window.model })),
  ]
  const resetIn = (at: number) => {
    const minutes = Math.max(0, Math.round((at * 1000 - now) / 60_000))
    const format = new Intl.RelativeTimeFormat(i18n.language, { numeric: 'auto', style: 'narrow' })
    return minutes >= 1440 ? format.format(Math.round(minutes / 1440), 'day')
      : minutes >= 60 ? format.format(Math.round(minutes / 60), 'hour')
        : format.format(minutes, 'minute')
  }
  if (compact) {
    // 列表行：各窗口横排成一组小仪表，每个三层——「窗口名 + 百分比 / 细条 / 距重置」。
    // 竖着排一个窗口占一行，订阅账号有三个窗口（5 小时 / 7 天 / 按模型的周限额）就把行撑成三四行高；
    // 横排后整组正好是两行文字的高度，和这一行其余各格同高，各渠道的同一个窗口也在同一竖列上，能上下比着看。
    return (
      <div className="flex w-full min-w-0 flex-col gap-1">
        {quota.allowed === false && <p className="text-[11px] leading-4 font-medium text-destructive">{t('admin:channelQuotaBlocked')}</p>}
        <div data-slot="quota-meters" className="grid w-full max-w-[26rem] auto-cols-fr grid-flow-col gap-x-3 min-[1420px]:gap-x-4">
          {rows.map((window) => {
            const stale = window.resets_at !== null && window.resets_at * 1000 <= now
            const percent = Math.min(100, Math.max(0, window.used_percent))
            // 仪表上的窗口名用短写（5h / 7d），和状态列的「近 24h」同一种写法；全称在悬停提示与读屏名里
            const fullLabel = windowLabel(window.window_secs)
            const meterName = window.scope ? `${window.scope} · ${fullLabel}` : fullLabel
            const reset = !stale && window.resets_at !== null ? resetSpan(window.resets_at, now) : null
            // 渠道配的百分比上限管的是账号窗口，按模型分的周限额不带。仪表最下一行只放剩余时长，具体重置时刻进悬停提示
            const limit = window.window_secs !== null && !window.scope ? limits?.[String(window.window_secs)] : undefined
            return (
              <CompactMeter key={window.name} slot="quota-meter" label={window.scope ?? shortWindowLabel(window.window_secs)} title={meterName}
                meterLabel={t('admin:channelQuotaMeterLabel', { window: meterName })} percent={stale ? null : percent}
                marker={typeof limit === 'number' ? limit : undefined}
                markerTitle={typeof limit === 'number' ? t('admin:channelQuotaLimitMarker', { percent: limit }) : undefined}
                footerTitle={reset?.full}
                footer={stale ? t('admin:channelQuotaRefreshing') : reset ? <>
                  <RotateCw aria-label={t('admin:channelQuotaResetLabel')} className="h-2.5 w-2.5 shrink-0 opacity-80" />
                  <span className="truncate">{reset.span}</span>
                </> : '\u00a0'} />
            )
          })}
        </div>
      </div>
    )
  }
  return (
    <div className="flex flex-col gap-2.5">
      {quota.allowed === false && <p className="text-xs text-destructive">{t('admin:channelQuotaBlocked')}</p>}
      {rows.map((window) => {
        const stale = window.resets_at !== null && window.resets_at * 1000 <= now
        const percent = Math.min(100, Math.max(0, window.used_percent))
        const limit = window.window_secs !== null && !window.scope ? limits?.[String(window.window_secs)] : undefined
        const tone = quotaTone(percent)
        const label = window.scope ? `${window.scope} · ${windowLabel(window.window_secs)}` : windowLabel(window.window_secs)
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
