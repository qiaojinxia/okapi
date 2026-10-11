import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Segmented } from '@/components/ui/segmented'
import { EmptyState } from '@/components/ui/state'
import { TimeChart } from '@/components/ui/time-chart'
import type { BreakdownResp, BreakdownRow } from './types'
import { calendarDays, modelColors, usageChart, USAGE_METRICS } from './usage-chart-data'
import { aggregateCacheHit } from './cache-metrics'
import type { UsageMetric } from './usage-chart-data'

/// 总览里只给最常看的三种口径；完整的七种在「用量趋势」页签。
const QUICK_METRICS = ['amount', 'tokens', 'requests'] as const satisfies readonly UsageMetric[]

/// `Etc/UTC` 这类 IANA 写法对用户没有意义，直接说 UTC。
export function zoneLabel(zone: string | undefined): string {
  if (!zone) return ''
  return zone === 'Etc/UTC' || zone === 'UTC' || zone === 'Etc/GMT' ? 'UTC' : zone
}

export function SpendTrendView({ rows, days, window, metric, onMetricChange, onExpand, quick = false }: {
  rows: BreakdownRow[]; days: number; window?: BreakdownResp['window']; metric: UsageMetric
  onMetricChange?: (metric: UsageMetric) => void
  onExpand?: () => void
  /// 总览卡片：三种常用口径、不带表格 / 导出控件、不重复口径说明。
  quick?: boolean
}) {
  const { t, i18n } = useTranslation()
  const end = window?.end_date ?? new Date().toISOString().slice(0, 10)
  const start = window?.start_date ?? new Date(new Date(`${end}T00:00:00Z`).getTime() - (days - 1) * 86400_000).toISOString().slice(0, 10)
  const shown: UsageMetric = quick && !QUICK_METRICS.includes(metric as (typeof QUICK_METRICS)[number]) ? 'amount' : metric
  const money = shown === 'amount' || shown === 'original'
  const ratio = shown === 'cache' || shown === 'success'
  const format = (v: number) => money ? new Intl.NumberFormat(i18n.language, { style: 'currency', currency: 'USD', maximumFractionDigits: Math.abs(v) > 0 && Math.abs(v) < 0.01 ? 4 : 2 }).format(v)
    : `${v.toLocaleString(i18n.language, { maximumFractionDigits: ratio ? 2 : 0 })}${ratio ? '%' : shown === 'latency' ? ' ms' : ''}`
  const chart = usageChart(rows, calendarDays(start, end), shown, t('analytics:other'), { total: t(`charts:metric_${shown}`), latency: t('charts:metric_latency'), ttft: t('analytics:ttft') }, modelColors(rows))
  // 今天还没过完：最后一根柱子是「截至目前」，画浅一些
  const partial = window && window.end_date === window.today ? window.today : undefined
  const options = (quick ? QUICK_METRICS : USAGE_METRICS).map((value) => ({ value, label: t(`charts:metric_${value}`) }))
  return (
    <Card className="flex min-w-0 flex-col rounded-xl"><CardContent className="flex flex-1 flex-col space-y-2 px-4 py-3">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h2 className="font-semibold">{t('charts:usageTrend')}</h2>
          <p className="mt-1 flex flex-wrap items-center gap-x-2 text-xs text-muted-foreground">
            <span>{start} — {end}{window?.timezone ? ` · ${zoneLabel(window.timezone)}` : ''}</span>
            {onExpand && <Button variant="link" size="xs" className="h-auto p-0 text-xs" onClick={onExpand}>{t('portal:expandTrend')}</Button>}
          </p>
        </div>
        {onMetricChange && <Segmented ariaLabel={t('charts:metric')} size="sm" value={shown} onChange={onMetricChange} options={options} />}
      </div>
      {rows.length === 0 ? <EmptyState hint={t('portal:emptyUsageHint')} />
        : <TimeChart compact controls={!quick} key={shown} {...chart} partialBucket={partial} percent={ratio} format={format} unit={money ? 'USD' : ratio ? '%' : shown === 'latency' ? 'ms' : t(`charts:metric_${shown}`)} label={t('charts:usageTrend')} />}
      {!quick && <p className="text-xs leading-5 text-muted-foreground">{shown === 'latency' ? t('charts:missingPerformance') : shown === 'cache' && aggregateCacheHit(rows).partial ? t('charts:partialCacheSamples') : ratio ? t('charts:ratioGaps') : t('charts:zeroDays')}</p>}
      {window && !quick && <p className="text-xs text-muted-foreground">{t('charts:freshness', { time: window.generated_at })} · {zoneLabel(window.timezone)}</p>}
    </CardContent></Card>
  )
}
