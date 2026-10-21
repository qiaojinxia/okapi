import { useTranslation } from 'react-i18next'
import { Card, CardContent } from '@/components/ui/card'
import { Segmented } from '@/components/ui/segmented'
import { Select } from '@/components/ui/select'
import { EmptyState } from '@/components/ui/state'
import { TimeChart } from '@/components/ui/time-chart'
import { cn } from '@/lib/utils'
import type { BreakdownResp, BreakdownRow } from './types'
import { calendarDays, modelColors, usageChart, USAGE_METRICS } from './usage-chart-data'
import { aggregateCacheHit } from './cache-metrics'
import type { UsageMetric } from './usage-chart-data'

/// 最常看的三种口径直接摆出来，其余收进「更多」下拉。
const QUICK_METRICS = ['amount', 'tokens', 'requests'] as const satisfies readonly UsageMetric[]
const MORE_METRICS = USAGE_METRICS.filter((m) => !(QUICK_METRICS as readonly string[]).includes(m))

/// `Etc/UTC` 这类 IANA 写法对用户没有意义，直接说 UTC。
export function zoneLabel(zone: string | undefined): string {
  if (!zone) return ''
  return zone === 'Etc/UTC' || zone === 'UTC' || zone === 'Etc/GMT' ? 'UTC' : zone
}

export function SpendTrendView({ rows, days, window, metric, onMetricChange, className }: {
  rows: BreakdownRow[]; days: number; window?: BreakdownResp['window']; metric: UsageMetric
  onMetricChange: (metric: UsageMetric) => void
  className?: string
}) {
  const { t, i18n } = useTranslation()
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
  return (
    <Card id="portal-trend" className={cn('flex min-w-0 scroll-mt-4 flex-col rounded-xl', className)}><CardContent className="flex flex-1 flex-col space-y-2 px-4 py-3 lg:min-h-0">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h2 className="font-semibold">{t('charts:usageTrend')}</h2>
          <p className="mt-1 text-xs text-muted-foreground">{start} — {end}{window?.timezone ? ` · ${zoneLabel(window.timezone)}` : ''}</p>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <Segmented ariaLabel={t('charts:metric')} size="sm" value={metric} onChange={onMetricChange} options={QUICK_METRICS.map((value) => ({ value, label: t(`charts:metric_${value}`) }))} />
          <Select aria-label={t('charts:moreMetrics')} className="w-32 [&_select]:h-8 [&_select]:text-xs" placeholder={t('charts:moreMetrics')}
            value={(MORE_METRICS as readonly string[]).includes(metric) ? metric : ''}
            onChange={(value) => value && onMetricChange(value as UsageMetric)}
            options={MORE_METRICS.map((value) => ({ value, label: t(`charts:metric_${value}`) }))} />
        </div>
      </div>
      {rows.length === 0 ? <EmptyState hint={t('portal:emptyUsageHint')} />
        : <TimeChart compact fill minPlotHeight={176} key={metric} {...chart} partialBucket={partial} percent={ratio} format={format} unit={money ? 'USD' : ratio ? '%' : metric === 'latency' ? 'ms' : t(`charts:metric_${metric}`)} label={t('charts:usageTrend')} />}
      {note && <p className="text-xs leading-5 text-muted-foreground">{note}</p>}
    </CardContent></Card>
  )
}
