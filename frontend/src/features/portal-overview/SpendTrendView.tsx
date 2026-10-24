import { useTranslation } from 'react-i18next'
import { Card } from '@/components/ui/card'
import { Segmented } from '@/components/ui/segmented'
import { Select } from '@/components/ui/select'
import { EmptyState } from '@/components/ui/state'
import { TimeChart } from '@/components/ui/time-chart'
import { formatBp, formatCount } from '@/lib/money'
import { cn } from '@/lib/utils'
import type { BreakdownResp, BreakdownRow, BreakdownTotal } from './types'
import { calendarDays, modelColors, usageChart, USAGE_METRICS } from './usage-chart-data'
import { aggregateCacheHit } from './cache-metrics'
import { formatDuration, formatUsd } from './format'
import type { UsageMetric } from './usage-chart-data'

/// 最常看的三种口径直接摆出来，其余收进「更多」下拉。
const QUICK_METRICS = ['amount', 'tokens', 'requests'] as const satisfies readonly UsageMetric[]
const MORE_METRICS = USAGE_METRICS.filter((m) => !(QUICK_METRICS as readonly string[]).includes(m))

/// `Etc/UTC` 这类 IANA 写法对用户没有意义，直接说 UTC。
export function zoneLabel(zone: string | undefined): string {
  if (!zone) return ''
  return zone === 'Etc/UTC' || zone === 'UTC' || zone === 'Etc/GMT' ? 'UTC' : zone
}

/// 用量趋势：卡片头是标题、口径页签和图表自带的控件（图表类型、数据表、导出），
/// 下面一行大字写这段时间该口径的合计（比率类口径给整段的汇总值），再是图。
export function SpendTrendView({ rows, total, days, window, metric, onMetricChange, className }: {
  rows: BreakdownRow[]; total: BreakdownTotal; days: number; window?: BreakdownResp['window']; metric: UsageMetric
  onMetricChange: (metric: UsageMetric) => void
  className?: string
}) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const end = window?.end_date ?? new Date().toISOString().slice(0, 10)
  const start = window?.start_date ?? new Date(new Date(`${end}T00:00:00Z`).getTime() - (days - 1) * 86400_000).toISOString().slice(0, 10)
  const money = metric === 'amount' || metric === 'original'
  const ratio = metric === 'cache' || metric === 'success'
  const format = (v: number) => money ? new Intl.NumberFormat(i18n.language, { style: 'currency', currency: 'USD', maximumFractionDigits: Math.abs(v) > 0 && Math.abs(v) < 0.01 ? 4 : 2 }).format(v)
    : `${v.toLocaleString(i18n.language, { maximumFractionDigits: ratio ? 2 : 0 })}${ratio ? '%' : metric === 'latency' ? ' ms' : ''}`
  const chart = usageChart(rows, calendarDays(start, end), metric, t('analytics:other'), { total: t(`charts:metric_${metric}`), latency: t('charts:metric_latency'), ttft: t('analytics:ttft') }, modelColors(rows))
  // 今天还没过完：最后一根柱子是「截至目前」，画浅一些
  const partial = window && window.end_date === window.today ? window.today : undefined
  // 只在口径本身有需要解释的地方加一句，常规口径不重复说明
  const note = metric === 'latency' ? t('charts:missingPerformance') : metric === 'cache' && aggregateCacheHit(rows).partial ? t('charts:partialCacheSamples') : ratio ? t('charts:ratioGaps') : null
  const headline = metric === 'amount' ? formatUsd(total.amount_micro, locale)
    : metric === 'original' ? formatUsd(total.original_micro ?? total.amount_micro + total.discount_micro, locale)
      : metric === 'requests' ? formatCount(total.requests, locale)
        : metric === 'tokens' ? formatCount(total.tokens, locale)
          : metric === 'cache' ? (() => { const hit = aggregateCacheHit(rows); return hit.bp == null ? '—' : formatBp(hit.bp, locale) })()
            : metric === 'success' ? (total.success_rate_bp == null ? '—' : formatBp(total.success_rate_bp, locale))
              : total.avg_latency_ms == null ? '—' : formatDuration(total.avg_latency_ms, locale)
  const title = t('charts:usageTrend')
  const head = (controls?: React.ReactNode) => <>
    <div className="flex flex-wrap items-center gap-3">
      <h2 className="text-[15px] font-semibold">{title}</h2>
      <Segmented ariaLabel={t('charts:metric')} size="sm" value={metric} onChange={onMetricChange} options={QUICK_METRICS.map((value) => ({ value, label: t(`charts:metric_${value}`) }))} />
      <Select aria-label={t('charts:moreMetrics')} className="w-32 [&_select]:h-8 [&_select]:text-xs" placeholder={t('charts:moreMetrics')}
        value={(MORE_METRICS as readonly string[]).includes(metric) ? metric : ''}
        onChange={(value) => value && onMetricChange(value as UsageMetric)}
        options={MORE_METRICS.map((value) => ({ value, label: t(`charts:metric_${value}`) }))} />
      {controls && <div className="ml-auto">{controls}</div>}
    </div>
    <p className="flex items-baseline gap-2.5">
      <span data-slot="trend-total" className="text-[22px] leading-8 font-semibold tabular-nums">{headline}</span>
      <span className="text-xs text-muted-foreground/80">{t(`charts:metric_${metric}`)}{money ? ' · USD' : ''}</span>
    </p>
  </>
  return (
    <Card id="portal-trend" className={cn('flex min-w-0 scroll-mt-4 flex-col gap-4 rounded-xl border-border px-5 py-5 shadow-none', className)}>
      {rows.length === 0 ? <>{head()}<EmptyState hint={t('portal:emptyUsageHint')} /></>
        : <TimeChart compact fill minPlotHeight={260} resetKey={metric} header={head} {...chart} partialBucket={partial} percent={ratio} format={format} unit={money ? 'USD' : ratio ? '%' : metric === 'latency' ? 'ms' : t(`charts:metric_${metric}`)} label={title} />}
      {note && <p className="text-xs leading-5 text-muted-foreground">{note}</p>}
    </Card>
  )
}
