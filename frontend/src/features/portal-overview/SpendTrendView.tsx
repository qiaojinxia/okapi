import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Card, CardContent } from '@/components/ui/card'
import { Segmented } from '@/components/ui/segmented'
import { EmptyState } from '@/components/ui/state'
import { TimeChart } from '@/components/ui/time-chart'
import type { BreakdownResp, BreakdownRow } from './types'
import { calendarDays, usageChart, USAGE_METRICS } from './usage-chart-data'
import type { UsageMetric } from './usage-chart-data'

export function SpendTrendView({ rows, days, window }: { rows: BreakdownRow[]; days: number; window?: BreakdownResp['window'] }) {
  const { t, i18n } = useTranslation()
  const [metric, setMetric] = useState<UsageMetric>('amount')
  const end = window?.end_date ?? new Date().toISOString().slice(0, 10)
  const start = window?.start_date ?? new Date(new Date(`${end}T00:00:00Z`).getTime() - (days - 1) * 86400_000).toISOString().slice(0, 10)
  const money = metric === 'amount' || metric === 'original'
  const ratio = metric === 'cache' || metric === 'success'
  const format = (v: number) => money ? new Intl.NumberFormat(i18n.language, { style: 'currency', currency: 'USD', maximumFractionDigits: 4 }).format(v)
    : `${v.toLocaleString(i18n.language, { maximumFractionDigits: ratio ? 2 : 0 })}${ratio ? '%' : metric === 'latency' ? ' ms' : ''}`
  const chart = usageChart(rows, calendarDays(start, end), metric, t('analytics:other'), { total: t(`charts:metric_${metric}`), latency: t('charts:metric_latency'), ttft: t('analytics:ttft') })
  return (
    <Card><CardContent className="space-y-4 pt-5">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div><h2 className="font-semibold">{t('charts:usageTrend')}</h2><p className="mt-1 text-xs text-muted-foreground">{start} — {end}{window?.timezone ? ` · ${window.timezone}` : ''}</p></div>
        <Segmented ariaLabel={t('charts:metric')} size="sm" value={metric} onChange={setMetric} options={USAGE_METRICS.map((value) => ({ value, label: t(`charts:metric_${value}`) }))} />
      </div>
      {rows.length === 0 ? <EmptyState hint={t('portal:emptyUsageHint')} /> : <TimeChart key={metric} {...chart} percent={ratio} format={format} unit={money ? 'USD' : ratio ? '%' : metric === 'latency' ? 'ms' : t(`charts:metric_${metric}`)} label={t('charts:usageTrend')} />}
      <p className="text-xs leading-5 text-muted-foreground">{metric === 'latency' ? t('charts:missingPerformance') : ratio ? t('charts:ratioGaps') : t('charts:zeroDays')}</p>
      {window && <p className="text-xs text-muted-foreground">{t('charts:freshness', { time: window.generated_at })} · {window.timezone}</p>}
    </CardContent></Card>
  )
}
