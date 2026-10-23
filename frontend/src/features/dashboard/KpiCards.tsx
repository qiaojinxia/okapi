import { Link } from '@tanstack/react-router'
import { Activity, AlertTriangle, ArrowUpRight, Coins, Cpu, Users } from 'lucide-react'
import type { LucideIcon } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { Stat } from '@/components/ui/stat'
import type { DateRange } from '@/components/ui/date-range'
import { ErrorState } from '@/components/ui/state'
import { useDashboardOverview, useDashboardPreviousOverview } from './data'
import { dashboardPeriodLabel, dashboardSearch } from './period'
import { describeError } from '@/lib/i18n'
import { formatBp, formatCount, formatMoneyAggregate } from '@/lib/money'
import type { AnalyticsSearch } from '@/features/analytics/route-state'

/// 主数字跟随统计口径，副行是和上一周期的对照；口径定义放进标签的悬停提示，不占副行。
/// 图标按指标分色引导视线，Token 用绿色最先抓住注意力。
function Kpi({
  icon,
  accent,
  period,
  label,
  hint,
  today,
  window,
  tone,
  loading,
  search,
  detail,
}: {
  icon: LucideIcon
  /// 图标块的色相：五张卡各用一种；Token 是主信息，图标和数值都用绿色。超阈值的错误率仍由 tone 染色。
  accent?: string
  /// 时段已在页头，卡片上不再重复显示；读屏和链接名仍带上，脱离页头也知道是哪段时间
  period: string
  label: string
  hint: string
  today: string
  window: React.ReactNode
  tone?: 'default' | 'good' | 'warn' | 'bad'
  loading: boolean
  search: AnalyticsSearch
  detail?: string
}) {
  const { t } = useTranslation()
  // 平板 6 栏：前三张各占 2 栏、后两张各占 3 栏，五张卡正好铺满两行，不留空位；桌面回到五栏一行
  const className = 'min-w-0 rounded-2xl last:col-span-2 md:col-span-2 md:nth-4:col-span-3 md:last:col-span-3 lg:col-span-1 lg:nth-4:col-span-1 lg:last:col-span-1'
  const sub = <span className="max-w-full rounded-md bg-muted/45 px-1.5 py-0.5 leading-4 lg:truncate lg:py-0 lg:[@media(min-height:900px)]:py-0.5">{window}</span>
  const title = <span title={hint}><span className="sr-only">{t('admin:dashboardMetricLabel', { period, label: '' })}</span>{label}</span>
  if (loading) return <Stat layout="stacked" compact className={`${className} border-border/70 bg-linear-to-br from-card to-muted/20 lg:py-2`} icon={icon} label={title} value={today} sub={sub} loading />
  return <Link to="/admin/stats" search={search} aria-label={t('admin:dashboardOpenMetric', { label: t('admin:dashboardMetricLabel', { period, label }), value: today })}
    className={`${className} group block outline-none focus-visible:ring-2 focus-visible:ring-primary/50 focus-visible:ring-offset-2 focus-visible:ring-offset-background`}>
    <Stat layout="stacked" compact className="h-full min-w-0 rounded-2xl border-border/70 bg-linear-to-br from-card via-card to-muted/25 transition-[border-color,box-shadow,transform] group-hover:-translate-y-0.5 group-hover:border-primary/35 group-hover:shadow-popover group-focus-visible:border-primary/40 lg:py-2 lg:max-xl:px-2" icon={icon}
      label={title}
      value={<span className="flex min-w-0 flex-wrap items-center gap-x-1.5 text-xl leading-8 sm:text-2xl lg:pr-9 lg:text-xl lg:leading-7 2xl:text-2xl 2xl:leading-8"><span className="min-w-0" title={today}>{today}</span>{detail && <span className="text-[11px] leading-4 font-normal text-muted-foreground">{detail}</span>}<ArrowUpRight aria-hidden className="hidden h-3.5 w-3.5 shrink-0 text-muted-foreground opacity-0 group-hover:text-primary group-hover:opacity-100 group-focus-visible:text-primary group-focus-visible:opacity-100 sm:block" /></span>}
      sub={sub} tone={tone} iconClassName={accent ? `h-8 w-8 rounded-lg ${accent}` : undefined} />
  </Link>
}

/// 「较前 7 天 ▲12%」：涨跌只用箭头表达，不上色——请求多了不一定是坏事。上期为 0 时没有可比的基数。
function change(current: number, previous: number | undefined, locale: string): string | null {
  if (previous === undefined || previous <= 0) return null
  const value = (current - previous) / previous
  const pct = Math.abs(value * 100)
  const shown = pct >= 100 ? Math.round(pct).toLocaleString(locale) : pct.toLocaleString(locale, { maximumFractionDigits: pct < 10 ? 1 : 0 })
  return `${value > 0.0005 ? '▲' : value < -0.0005 ? '▼' : '·'} ${shown}%`
}

/// The main values and drill-down links always use the page's selected calendar.
/// Today compares against yesterday's full day as a neutral reference, never an incomplete-day delta;
/// longer windows compare against the previous window of the same length.
export function KpiCards({ days, range }: { days: number; range?: DateRange | null }) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const q = useDashboardOverview(days, range)
  const prev = useDashboardPreviousOverview(days, q.data?.calendar)

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
  const search = dashboardSearch(days, range)
  const isToday = days === 1 && !range
  /// 今天：昨日全天；更长窗口：较上一等长周期的变化，上期没有基数时退回口径说明。
  const reference = (value: string, delta: string | null, basis: string) => isToday
    ? t('admin:kpiYesterday', { value })
    : delta === null ? basis : t('admin:dashboardPeriodChange', { days, delta })
  const errorBp = cur?.error_rate_bp ?? 0
  const period = dashboardPeriodLabel(days, range, t)
  const errorSub = isToday
    ? t('admin:kpiYesterday', { value: (yday?.requests ?? 0) > 0 ? formatBp(yday?.error_rate_bp ?? 0, locale) : '—' })
    // 错误率本身是比例，和上期直接比数值，不算「涨了百分之几」
    : before && before.requests > 0 ? t('admin:dashboardPeriodValue', { days, value: formatBp(before.error_rate_bp, locale) }) : t('admin:dashboardErrorBasis')

  return (
    <div aria-busy={loading} className="grid min-w-0 grid-cols-2 gap-2 md:grid-cols-6 lg:grid-cols-5">
      <Kpi
        period={period}
        loading={loading}
        icon={Cpu}
        accent="bg-success/12 text-success"
        tone="good"
        label={t('admin:kpiTokens')}
        hint={t('admin:dashboardTokenBasis')}
        search={{ ...search, measure: 'tokens' }}
        today={formatCount(cur?.tokens ?? 0, locale)}
        window={reference(formatCount(yday?.tokens ?? 0, locale), cur ? change(cur.tokens, before?.tokens, locale) : null, t('admin:dashboardTokenBasis'))}
      />
      <Kpi
        period={period}
        loading={loading}
        icon={Activity}
        accent="bg-chart-1/12 text-chart-1"
        label={t('admin:kpiRequests')}
        hint={t('admin:dashboardRequestBasis')}
        search={{ ...search, measure: 'requests' }}
        today={formatCount(cur?.requests ?? 0, locale)}
        window={reference(formatCount(yday?.requests ?? 0, locale), cur ? change(cur.requests, before?.requests, locale) : null, t('admin:dashboardRequestBasis'))}
      />
      <Kpi
        period={period}
        loading={loading}
        icon={AlertTriangle}
        accent={errorBp >= 500 ? 'bg-destructive/12 text-destructive' : 'bg-warning/14 text-warning'}
        label={t('admin:kpiErrorRate')}
        hint={t('admin:dashboardErrorBasis')}
        search={{ ...search, measure: 'error_rate' }}
        today={(cur?.requests ?? 0) > 0 ? formatBp(errorBp, locale) : '—'}
        detail={(cur?.requests ?? 0) > 0 ? t('admin:dashboardFailedCalls', { n: formatCount(cur?.errors ?? 0, locale) }) : undefined}
        window={errorSub}
        // 阈值与渠道健康卡一致：1% 起提醒，5% 起告警
        tone={errorBp >= 500 ? 'bad' : errorBp >= 100 ? 'warn' : 'default'}
      />
      <Kpi
        period={period}
        loading={loading}
        icon={Coins}
        accent="bg-chart-3/12 text-chart-3"
        label={t('admin:kpiRevenue')}
        hint={t('admin:dashboardRevenueBasis')}
        search={{ ...search, measure: 'amount' }}
        today={formatMoneyAggregate(cur?.amount_micro ?? 0, locale)}
        window={reference(formatMoneyAggregate(yday?.amount_micro ?? 0, locale), cur ? change(cur.amount_micro, before?.amount_micro, locale) : null, t('admin:dashboardRevenueBasis'))}
      />
      <Kpi
        period={period}
        loading={loading}
        icon={Users}
        accent="bg-chart-2/12 text-chart-2"
        label={t('admin:kpiActiveUsers')}
        hint={t('admin:dashboardUserBasis')}
        search={{ ...search, view: 'breakdown', by: 'user' }}
        today={formatCount(cur?.active_users ?? 0, locale)}
        window={reference(formatCount(yday?.active_users ?? 0, locale), cur ? change(cur.active_users, before?.active_users, locale) : null, t('admin:dashboardUserBasis'))}
      />
    </div>
  )
}
