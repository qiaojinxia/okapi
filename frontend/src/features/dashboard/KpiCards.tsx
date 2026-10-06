import { Link } from '@tanstack/react-router'
import { Activity, AlertTriangle, ArrowUpRight, Coins, Cpu, Users } from 'lucide-react'
import type { LucideIcon } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { Stat } from '@/components/ui/stat'
import type { DateRange } from '@/components/ui/date-range'
import { ErrorState } from '@/components/ui/state'
import { useDashboardOverview } from './data'
import { dashboardPeriodLabel, dashboardSearch } from './period'
import { describeError } from '@/lib/i18n'
import { formatBp, formatCount, formatMoneyAggregate } from '@/lib/money'
import type { AnalyticsSearch } from '@/features/analytics/route-state'

/// 主数字跟随统计口径，副行提供对照；使用统一 Stat 保持指标样式一致。
function Kpi({
  icon,
  label,
  shortLabel,
  today,
  window,
  tone,
  accent,
  loading,
  search,
  detail,
}: {
  icon: LucideIcon
  label: string
  shortLabel: string
  today: string
  window: React.ReactNode
  tone?: 'default' | 'warn' | 'bad'
  /// 图标块的色相：五张卡各用一种，不再是一排同色；超阈值的错误率仍由 tone 染红。
  accent?: string
  loading: boolean
  search: AnalyticsSearch
  detail?: string
}) {
  const { t } = useTranslation()
  // 副行只放口径说明或昨日全天参考，窄卡允许换行，不截断关键数字。
  // 平板 6 栏：前三张各占 2 栏、后两张各占 3 栏，五张卡正好铺满两行，不留空位；桌面回到五栏一行
  const className = 'min-w-0 rounded-xl last:col-span-2 md:col-span-2 md:nth-4:col-span-3 md:last:col-span-3 lg:col-span-1 lg:nth-4:col-span-1 lg:last:col-span-1'
  const sub = <span className="max-w-full lg:truncate" title={typeof window === 'string' ? window : undefined}>{window}</span>
  if (loading) return <Stat layout="stacked" compact className={`${className} lg:py-2`} icon={icon} label={label} value={today} sub={sub} loading />
  return <Link to="/admin/stats" search={search} aria-label={t('admin:dashboardOpenMetric', { label, value: today })}
    className={`${className} group block outline-none focus-visible:ring-2 focus-visible:ring-primary/50 focus-visible:ring-offset-2 focus-visible:ring-offset-background`}>
    <Stat layout="stacked" compact className="h-full min-w-0 rounded-xl transition-colors group-hover:border-primary/40 group-hover:bg-accent/30 group-focus-visible:border-primary/40 lg:py-2 lg:max-xl:px-2" icon={icon}
      label={<><span className="md:hidden">{shortLabel}</span><span className="hidden md:inline">{label}</span></>}
      value={<span className="flex min-w-0 flex-wrap items-center gap-x-1.5 text-lg sm:text-xl"><span className="min-w-0" title={today}>{today}</span>{detail && <span className="text-[11px] font-normal text-muted-foreground">{detail}</span>}<ArrowUpRight aria-hidden className="hidden h-3.5 w-3.5 shrink-0 text-muted-foreground group-hover:text-primary group-focus-visible:text-primary sm:block" /></span>}
      sub={sub} tone={tone} iconClassName={accent} />
  </Link>
}

/// The main values and drill-down links always use the page's selected calendar.
/// Yesterday's full day is a neutral reference, never an incomplete-day delta.
export function KpiCards({ days, range }: { days: number; range?: DateRange | null }) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const q = useDashboardOverview(days, range)

  if (q.isError) {
    return <ErrorState message={describeError(q.error)} onRetry={() => void q.refetch()} />
  }
  if (range && q.isSuccess && (q.data.calendar?.start_date !== range.start || q.data.calendar?.end_date !== range.end)) {
    return <ErrorState message={t('admin:dashboardRangeUnavailable')} onRetry={() => void q.refetch()} />
  }

  const loading = q.isPending
  const today = q.data?.window
  const yday = q.data?.yesterday
  const search = dashboardSearch(days, range)
  const isToday = days === 1 && !range
  const reference = (value: string, explanation: string) => isToday ? t('admin:kpiYesterday', { value }) : explanation
  const errorBp = today?.error_rate_bp ?? 0
  const period = dashboardPeriodLabel(days, range, t)
  const scopedLabel = (label: string) => t('admin:dashboardMetricLabel', { period, label })

  return (
    <div aria-busy={loading} className="grid min-w-0 grid-cols-2 gap-2 md:grid-cols-6 lg:grid-cols-5">
      <Kpi
        icon={Cpu}
        accent="bg-chart-4/12 text-chart-4"
        loading={loading}
        label={scopedLabel(t('admin:kpiTokens'))}
        shortLabel={t('admin:kpiTokens')}
        search={{ ...search, measure: 'tokens' }}
        today={formatCount(today?.tokens ?? 0, locale)}
        window={reference(
          formatCount(yday?.tokens ?? 0, locale),
          t('admin:dashboardTokenBasis'),
        )}
      />
      <Kpi
        icon={Activity}
        accent="bg-chart-1/12 text-chart-1"
        loading={loading}
        label={scopedLabel(t('admin:kpiRequests'))}
        shortLabel={t('admin:kpiRequests')}
        search={{ ...search, measure: 'requests' }}
        today={formatCount(today?.requests ?? 0, locale)}
        window={reference(
          formatCount(yday?.requests ?? 0, locale),
          t('admin:dashboardRequestBasis'),
        )}
      />
      <Kpi
        icon={AlertTriangle}
        accent={errorBp < 100 ? 'bg-warning/14 text-warning' : undefined}
        loading={loading}
        label={scopedLabel(t('admin:kpiErrorRate'))}
        shortLabel={t('admin:kpiErrorRate')}
        search={{ ...search, measure: 'error_rate' }}
        today={(today?.requests ?? 0) > 0 ? formatBp(errorBp, locale) : '—'}
        detail={(today?.requests ?? 0) > 0 ? t('admin:dashboardFailedCalls', { n: formatCount(today?.errors ?? 0, locale) }) : undefined}
        window={reference(
          (yday?.requests ?? 0) > 0 ? formatBp(yday?.error_rate_bp ?? 0, locale) : '—',
          t('admin:dashboardErrorBasis'),
        )}
        // 阈值与渠道健康卡一致：1% 起提醒，5% 起告警
        tone={errorBp >= 500 ? 'bad' : errorBp >= 100 ? 'warn' : 'default'}
      />
      <Kpi
        icon={Coins}
        accent="bg-success/12 text-success"
        loading={loading}
        label={scopedLabel(t('admin:kpiRevenue'))}
        shortLabel={t('admin:kpiRevenue')}
        search={{ ...search, measure: 'amount' }}
        today={formatMoneyAggregate(today?.amount_micro ?? 0, locale)}
        window={reference(
          formatMoneyAggregate(yday?.amount_micro ?? 0, locale),
          t('admin:dashboardRevenueBasis'),
        )}
      />
      <Kpi
        icon={Users}
        accent="bg-chart-2/12 text-chart-2"
        loading={loading}
        label={scopedLabel(t('admin:kpiActiveUsers'))}
        shortLabel={t('admin:kpiActiveUsers')}
        search={{ ...search, view: 'breakdown', by: 'user' }}
        today={formatCount(today?.active_users ?? 0, locale)}
        window={reference(
          formatCount(yday?.active_users ?? 0, locale),
          t('admin:dashboardUserBasis'),
        )}
      />
    </div>
  )
}
