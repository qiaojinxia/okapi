import { useTranslation } from 'react-i18next'
import { EmptyState, ErrorState, LoadingState } from '@/components/ui/state'
import { Segmented } from '@/components/ui/segmented'
import { TimeChart } from '@/components/ui/time-chart'
import type { DashboardTrend } from './types'
import { describeError } from '@/lib/i18n'
import { formatCount, formatMoneyAggregate } from '@/lib/money'
import { useDashboardChart } from './data'
import { trendChart } from '@/features/analytics/trend-data'
import type { DateRange } from '@/components/ui/date-range'

export function TrendCard({ days, range, metric, onMetricChange }: { days: number; range?: DateRange | null; metric: DashboardTrend; onMetricChange: (value: DashboardTrend) => void }) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  // 图表只要逐桶的请求 / 金额 / Token，走精简查询，不必等质量与 Token 构成的完整汇总。
  const q = useDashboardChart(days, range)
  const combined = metric === 'combined'
  const title = t(metric === 'tokens' ? 'admin:dashboardTokenTrend' : 'admin:trendTitle')
  // 三个视图读取同一次精简查询，空桶及小时粒度沿用统计页的日历处理。
  const values = (measure: 'amount' | 'requests' | 'tokens') => q.data?.data.length ? trendChart(q.data, measure, '', '', '', '').data : []
  const amount = values('amount')
  const tokens = values('tokens')
  const data = values('requests').map((point, i) => ({ bucket: point.bucket, requests: Number(point.value ?? 0), amount: Number(amount[i]?.value ?? 0), tokens: Number(tokens[i]?.value ?? 0) }))
  const selected = combined ? 'requests' : metric
  const label = t(`charts:metric_${selected}`)
  const money = (v: number) => new Intl.NumberFormat(locale, { style: 'currency', currency: 'USD', maximumFractionDigits: 4 }).format(v)
  const format = (v: number) => metric === 'amount'
    ? money(v)
    : v.toLocaleString(locale, { maximumFractionDigits: 1 })
  const requests = data.reduce((sum, point) => sum + point.requests, 0)
  const revenue = data.reduce((sum, point) => sum + point.amount, 0)
  const total = data.reduce((sum, point) => sum + point[selected], 0)
  const legendTotal = (value: number, measure: 'amount' | 'requests' | 'tokens') => measure === 'amount' ? formatMoneyAggregate(Math.round(value * 1_000_000), locale) : formatCount(value, locale)
  // 今天还没过完：最后一根柱子是「截至目前」，画浅一些并在图下说明
  const today = q.data?.granularity === 'day' ? q.data.window?.today : undefined
  const partial = today && data.some((point) => point.bucket === today) ? today : undefined
  // 单一指标时图下补两个读数：日均按完整时段（含空日、含今天）算，峰值带上日期 / 小时
  const hourly = q.data?.granularity === 'hour'
  const peak = data.reduce<(typeof data)[number] | undefined>((max, point) => !max || point[selected] > max[selected] ? point : max, undefined)
  const notes = [
    !combined && <span key="average">{t(hourly ? 'admin:dashboardHourlyAverage' : 'admin:dashboardTrendAverage')} <b className="font-semibold text-foreground">{format(total / Math.max(data.length, 1))}</b></span>,
    !combined && <span key="peak">{t(hourly ? 'admin:dashboardHourlyPeak' : 'admin:dashboardTrendPeak')} <b className="font-semibold text-foreground">{peak && peak[selected] > 0 ? `${format(peak[selected])} · ${peak.bucket.slice(5)}` : '—'}</b></span>,
    partial && <span key="partial">{t('admin:dashboardTodayPartial', { date: partial.slice(5) })}</span>,
  ].filter(Boolean)
  // 标题、指标页签与图表自带的控件（数据表、导出）同一行；图表没出来时只留前两项。
  const head = (controls?: React.ReactNode) => <div className="flex flex-wrap items-center gap-3">
    <h2 className="flex min-h-[34px] items-center text-[15px] font-semibold">{title}</h2>
    <span className="flex-1" />
    <Segmented size="sm" ariaLabel={t('charts:metric')} value={metric} onChange={onMetricChange} options={(['combined', 'requests', 'amount', 'tokens'] as const).map((value) => ({ value, label: value === 'combined' ? t('admin:dashboardCompareTrend') : t(`charts:metric_${value}`) }))} />
    {controls && <div className="ml-auto">{controls}</div>}
  </div>
  return <section data-slot="dashboard-trend" aria-label={title} className="flex min-w-0 flex-[2_1_35rem] flex-col gap-2.5 rounded-xl border border-border bg-card p-3.5">
    {q.isPending ? <>{head()}<LoadingState /></> : q.isError ? <>{head()}<ErrorState message={describeError(q.error)} onRetry={() => void q.refetch()} /></> : !q.data?.data.length ? <>{head()}<EmptyState hint={t('admin:trendEmptyHint')} /></> : <>
      <TimeChart compact fill minPlotHeight={240} key={metric} header={head} data={data} label={title} unit={metric === 'amount' ? 'USD' : label} format={format}
        secondaryAxis={combined ? { unit: 'USD', format: money } : undefined}
        partialBucket={partial}
        legendDetail={combined ? { requests: legendTotal(requests, 'requests'), amount: legendTotal(revenue, 'amount') } : { [selected]: legendTotal(total, selected) }}
        legendExtra={<span className="inline-flex min-h-8 items-center gap-1.5 px-2 text-xs text-muted-foreground">{t('admin:dashboardAverageCharge')}<b className="text-sm font-semibold text-foreground tabular-nums">{requests > 0 ? money(revenue / requests) : '—'}</b></span>}
        series={combined ? [
          { key: 'requests', label: t('admin:kpiRequests'), color: 'var(--color-primary)' },
          { key: 'amount', label: t('admin:kpiRevenue'), color: 'var(--color-success)', axis: 'right' },
        ] : [{ key: selected, label, color: metric === 'amount' ? 'var(--color-success)' : 'var(--color-primary)' }]} />
      {notes.length > 0 && <p className="flex flex-wrap gap-x-4 gap-y-0.5 text-xs text-muted-foreground/80">{notes}</p>}
    </>}
  </section>
}
