import { Link } from '@tanstack/react-router'
import { useTranslation } from 'react-i18next'
import { Skeleton } from '@/components/ui/skeleton'
import { Sparkline } from '@/components/ui/sparkline'
import type { DateRange } from '@/components/ui/date-range'
import { ErrorState } from '@/components/ui/state'
import type { AnalyticsSearch } from '@/features/analytics/route-state'
import { describeError } from '@/lib/i18n'
import { formatBp, formatCount, formatMoneyAggregate } from '@/lib/money'
import { cn } from '@/lib/utils'
import { useDashboardChart, useDashboardOverview, useDashboardPreviousOverview } from './data'
import { dashboardPeriodLabel, dashboardSearch } from './period'
import { sparkValues } from './sparkline-data'

/// 右上角的对照：`good` / `bad` 是语义（错误率涨是坏事），不是箭头方向；`neutral` 用于今天的昨日参照。
interface Aside { text: string; tone: 'good' | 'bad' | 'neutral' }
const ASIDE_TONE = { good: 'text-success', bad: 'text-destructive', neutral: 'text-muted-foreground' } as const

/// 一张指标卡：标签 + 对照、主数字 + 副行、右侧迷你趋势。整张卡是进入同窗分析的链接。
/// 时段已在页头，卡片上不重复显示；读屏和链接名仍带上，脱离页头也知道是哪段时间。
function Kpi({ loading, label, hint, period, value, valueClassName, aside, sub, spark, search }: {
  loading: boolean
  label: string
  hint: string
  period: string
  value: string
  valueClassName?: string
  aside?: Aside | null
  sub: string
  spark: number[]
  search: AnalyticsSearch
}) {
  const { t } = useTranslation()
  const card = 'flex min-w-0 flex-col gap-1.5 rounded-xl border border-border bg-card px-3.5 py-3'
  if (loading) return <div aria-hidden className={card}>
    <Skeleton className="h-4 w-16" />
    <Skeleton className="mt-0.5 h-[22px] w-28" />
    <Skeleton className="h-3.5 w-24" />
  </div>
  return <Link to="/admin/stats" search={search} aria-label={t('admin:dashboardOpenMetric', { label: t('admin:dashboardMetricLabel', { period, label }), value })}
    className={cn(card, 'group outline-none transition-colors hover:border-primary/40 focus-visible:ring-2 focus-visible:ring-primary/50')}>
    <span className="flex items-center justify-between gap-2 text-[13px] text-muted-foreground">
      <span title={hint} className="min-w-0 truncate">{label}</span>
      {aside && <span className={cn('shrink-0 text-xs font-medium tabular-nums', ASIDE_TONE[aside.tone])}>{aside.text}</span>}
    </span>
    <span className="flex items-end justify-between gap-3">
      <span className="flex min-w-0 flex-col gap-0.5">
        <span title={value} className={cn('text-[22px] leading-[1.1] font-bold tracking-tight tabular-nums [overflow-wrap:anywhere]', valueClassName)}>{value}</span>
        <span className="text-xs text-muted-foreground/80">{sub}</span>
      </span>
      <Sparkline values={spark} area={false} className="h-[26px] w-[72px] shrink-0" />
    </span>
  </Link>
}

/// 与上一周期相比的变化，上期为 0 时没有可比的基数。
function change(current: number, previous: number | undefined, locale: string): { text: string; sign: -1 | 0 | 1 } | null {
  if (previous === undefined || previous <= 0) return null
  const value = (current - previous) / previous
  const pct = Math.abs(value * 100)
  const shown = pct >= 100 ? Math.round(pct).toLocaleString(locale) : pct.toLocaleString(locale, { maximumFractionDigits: pct < 10 ? 1 : 0 })
  const sign = value > 0.0005 ? 1 : value < -0.0005 ? -1 : 0
  return { text: `${sign > 0 ? '▲' : sign < 0 ? '▼' : '·'} ${shown}%`, sign }
}

/// The main values and drill-down links always use the page's selected calendar.
/// Today compares against yesterday's full day as a neutral reference, never an incomplete-day delta;
/// longer windows compare against the previous window of the same length.
export function KpiCards({ days, range }: { days: number; range?: DateRange | null }) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const q = useDashboardOverview(days, range)
  const prev = useDashboardPreviousOverview(days, q.data?.calendar)
  const chart = useDashboardChart(days, range)

  if (q.isError) {
    return <ErrorState message={describeError(q.error)} onRetry={() => void q.refetch()} />
  }
  if (range && q.isSuccess && (q.data.calendar?.start_date !== range.start || q.data.calendar?.end_date !== range.end)) {
    return <ErrorState message={t('admin:dashboardRangeUnavailable')} onRetry={() => void q.refetch()} />
  }

  const loading = q.isPending
  const cur = q.data?.window
  const yday = q.data?.yesterday
  const before = prev.isSuccess ? prev.data.window : undefined
  const trend = chart.isError ? undefined : chart.data
  const search = dashboardSearch(days, range)
  const isToday = days === 1 && !range
  const period = dashboardPeriodLabel(days, range, t)
  /// 今天：昨日全天的数字（中性）；更长窗口：较上一等长周期的涨跌，上期没有基数时不给。
  const volumeAside = (now: number | undefined, last: number | undefined, yesterday: string): Aside | null => {
    if (isToday) return { text: t('admin:dashboardYesterdayShort', { value: yesterday }), tone: 'neutral' }
    const delta = now === undefined ? null : change(now, last, locale)
    return delta ? { text: delta.text, tone: delta.sign > 0 ? 'good' : delta.sign < 0 ? 'bad' : 'neutral' } : null
  }
  const compared = (aside: Aside | null | undefined, basis: string) => !isToday && aside ? t('admin:dashboardVsPrevious', { days }) : basis
  const tokensAside = volumeAside(cur?.tokens, before?.tokens, formatCount(yday?.tokens ?? 0, locale))
  const requestsAside = volumeAside(cur?.requests, before?.requests, formatCount(yday?.requests ?? 0, locale))
  const revenueAside = volumeAside(cur?.amount_micro, before?.amount_micro, formatMoneyAggregate(yday?.amount_micro ?? 0, locale))

  const hasRequests = (cur?.requests ?? 0) > 0
  const errorBp = cur?.error_rate_bp ?? 0
  // 错误率本身是比例，和上期直接比数值（百分点），不算「涨了百分之几」；涨是坏事
  const pointsChange = hasRequests && before && before.requests > 0 ? (errorBp - before.error_rate_bp) / 100 : null
  const errorAside: Aside | null = isToday
    ? { text: t('admin:dashboardYesterdayShort', { value: (yday?.requests ?? 0) > 0 ? formatBp(yday?.error_rate_bp ?? 0, locale) : '—' }), tone: 'neutral' }
    : pointsChange === null ? null
      : Math.abs(pointsChange) < 0.005 ? { text: '· 0pp', tone: 'neutral' }
        : { text: `${pointsChange > 0 ? '▲' : '▼'} ${Math.abs(pointsChange).toLocaleString(locale, { maximumFractionDigits: 2 })}pp`, tone: pointsChange > 0 ? 'bad' : 'good' }
  const errorSub = !isToday && before && before.requests > 0
    ? t('admin:dashboardPeriodValue', { days, value: formatBp(before.error_rate_bp, locale) })
    : t('admin:dashboardErrorBasis')

  return (
    <div aria-busy={loading} className="grid min-w-0 grid-cols-1 gap-2.5 @md:grid-cols-2 @5xl:grid-cols-4">
      <Kpi
        period={period}
        loading={loading}
        label={t('admin:kpiTokens')}
        hint={t('admin:dashboardTokenBasis')}
        search={{ ...search, measure: 'tokens' }}
        value={formatCount(cur?.tokens ?? 0, locale)}
        aside={tokensAside}
        sub={compared(tokensAside, t('admin:dashboardTokenBasis'))}
        spark={sparkValues(trend, 'tokens')}
      />
      <Kpi
        period={period}
        loading={loading}
        label={t('admin:kpiRequests')}
        hint={t('admin:dashboardRequestBasis')}
        search={{ ...search, measure: 'requests' }}
        value={formatCount(cur?.requests ?? 0, locale)}
        aside={requestsAside}
        sub={t('admin:dashboardFailedCalls', { n: formatCount(cur?.errors ?? 0, locale) })}
        spark={sparkValues(trend, 'requests')}
      />
      <Kpi
        period={period}
        loading={loading}
        label={t('admin:kpiRevenue')}
        hint={t('admin:dashboardRevenueBasis')}
        search={{ ...search, measure: 'amount' }}
        value={formatMoneyAggregate(cur?.amount_micro ?? 0, locale)}
        aside={revenueAside}
        sub={compared(revenueAside, t('admin:dashboardRevenueBasis'))}
        spark={sparkValues(trend, 'amount')}
      />
      <Kpi
        period={period}
        loading={loading}
        label={t('admin:kpiErrorRate')}
        hint={t('admin:dashboardErrorBasis')}
        search={{ ...search, measure: 'error_rate' }}
        value={hasRequests ? formatBp(errorBp, locale) : '—'}
        // 阈值与渠道健康卡一致：1% 起提醒，5% 起告警
        valueClassName={hasRequests && errorBp >= 500 ? 'text-destructive' : hasRequests && errorBp >= 100 ? 'text-warning' : undefined}
        aside={errorAside}
        sub={errorSub}
        spark={sparkValues(trend, 'error_rate')}
      />
    </div>
  )
}
