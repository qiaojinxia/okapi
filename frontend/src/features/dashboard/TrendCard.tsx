import { useTranslation } from 'react-i18next'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { EmptyState, ErrorState, LoadingState } from '@/components/ui/state'
import { Segmented } from '@/components/ui/segmented'
import { TimeChart } from '@/components/ui/time-chart'
import type { DashboardTrend } from './types'
import { describeError } from '@/lib/i18n'
import { useDashboardUsage } from './data'
import { trendChart } from '@/features/analytics/trend-data'

export function TrendCard({ days, metric, onMetricChange }: { days: number; metric: DashboardTrend; onMetricChange: (value: DashboardTrend) => void }) {
  const { t, i18n } = useTranslation()
  const q = useDashboardUsage(days)
  const end = q.data?.window?.end_date ?? q.data?.window?.end_at?.slice(0, 10)
  const start = q.data?.window?.start_date ?? q.data?.window?.start_at?.slice(0, 10)
  const combined = metric === 'combined'
  const title = t(metric === 'tokens' ? 'admin:dashboardTokenTrend' : 'admin:trendTitle')
  // 三个视图读取同一次汇总，空桶及小时粒度沿用统计页的日历处理。
  const values = (measure: 'amount' | 'requests' | 'tokens') => q.data?.data.length ? trendChart(q.data, measure, '', '', '', '').data : []
  const amount = values('amount')
  const tokens = values('tokens')
  const data = values('requests').map((point, i) => ({ bucket: point.bucket, requests: Number(point.value ?? 0), amount: Number(amount[i]?.value ?? 0), tokens: Number(tokens[i]?.value ?? 0) }))
  const selected = combined ? 'requests' : metric
  const label = t(`charts:metric_${selected}`)
  const money = (v: number) => new Intl.NumberFormat(i18n.language, { style: 'currency', currency: 'USD', maximumFractionDigits: 4 }).format(v)
  const format = (v: number) => metric === 'amount'
    ? money(v)
    : v.toLocaleString(i18n.language, { maximumFractionDigits: 1 })
  const total = data.reduce((sum, point) => sum + point[selected], 0)
  const revenue = data.reduce((sum, point) => sum + point.amount, 0)
  const peak = data.reduce<(typeof data)[number] | undefined>((max, point) => !max || point[selected] > max[selected] ? point : max, undefined)
  const summary = combined ? [
    [t('admin:kpiRequests'), format(total)],
    [t('admin:kpiRevenue'), money(revenue)],
    [t('admin:dashboardAverageCharge'), total > 0 ? money(revenue / total) : '—'],
  ] : [
    [t('admin:dashboardTrendTotal'), format(total)],
    [t(q.data?.granularity === 'hour' ? 'admin:dashboardHourlyAverage' : 'admin:dashboardTrendAverage'), format(total / Math.max(data.length, 1))],
    [t(q.data?.granularity === 'hour' ? 'admin:dashboardHourlyPeak' : 'admin:dashboardTrendPeak'), peak && peak[selected] > 0 ? `${format(peak[selected])} · ${peak.bucket.slice(5)}` : '—'],
  ]
  return <Card data-slot="dashboard-trend" className="flex min-w-0 flex-col rounded-xl">
    <CardHeader className="gap-1 px-4 pt-3 pb-2 lg:pt-2">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <CardTitle>{title}</CardTitle>
        <Segmented size="sm" ariaLabel={t('charts:metric')} value={metric} onChange={onMetricChange} options={(['combined', 'requests', 'amount', 'tokens'] as const).map((value) => ({ value, label: value === 'combined' ? t('admin:dashboardCompareTrend') : t(`charts:metric_${value}`) }))} />
      </div>
      <p className="text-xs leading-5 text-muted-foreground">{q.isSuccess && start && end ? `${start} — ${end}${q.data.window?.timezone ? ` · ${q.data.window.timezone}` : ''}` : t('admin:lastDays', { days })}</p>
    </CardHeader>
    <CardContent className="flex-1 px-4 pt-0 pb-3 lg:pb-2">
      {q.isPending ? <LoadingState /> : q.isError ? <ErrorState message={describeError(q.error)} onRetry={() => void q.refetch()} /> : !q.data?.data.length ? <EmptyState hint={t('admin:trendEmptyHint')} /> : <>
        <div className="mb-2 grid grid-cols-2 gap-2 rounded-lg bg-muted/40 px-3 py-2 sm:grid-cols-3 lg:mb-1 lg:flex lg:flex-wrap lg:justify-between lg:py-1.5">
          {summary.map(([name, value]) => <div key={name} className="min-w-0 last:col-span-2 sm:last:col-span-1 lg:flex lg:flex-wrap lg:items-baseline lg:gap-x-1.5"><p className="text-xs text-muted-foreground">{name}</p><p className="mt-1 break-words text-sm font-semibold tabular-nums lg:mt-0">{value}</p></div>)}
        </div>
        <TimeChart compact key={metric} data={data} label={title} unit={metric === 'amount' ? 'USD' : label} format={format}
          secondaryAxis={combined ? { unit: 'USD', format: money } : undefined}
          series={combined ? [
            { key: 'requests', label: t('admin:kpiRequests'), color: 'var(--color-primary)' },
            { key: 'amount', label: t('admin:kpiRevenue'), color: 'var(--color-success)', axis: 'right' },
          ] : [{ key: selected, label, color: metric === 'amount' ? 'var(--color-success)' : 'var(--color-primary)' }]} />
      </>}
    </CardContent>
  </Card>
}
