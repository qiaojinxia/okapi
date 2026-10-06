import { useCallback, useEffect, useState } from 'react'
import { useQueryClient } from '@tanstack/react-query'
import { getRouteApi, Link } from '@tanstack/react-router'
import { ArrowUpRight, CircleHelp, LayoutDashboard, RefreshCw } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { PageHeader } from '@/components/ui/page'
import { Button } from '@/components/ui/button'
import { Segmented } from '@/components/ui/segmented'
import { DateRangePicker } from '@/components/ui/date-range'
import { Tooltip } from '@/components/ui/tooltip'
import { AttentionStatus } from '@/features/dashboard/AttentionCard'
import { KpiCards } from '@/features/dashboard/KpiCards'
import { RealtimeCard } from '@/features/dashboard/RealtimeCard'
import { TrendCard } from '@/features/dashboard/TrendCard'
import { OperationsSummary } from '@/features/dashboard/OperationsSummary'
import { DistributionSummary } from '@/features/dashboard/DistributionSummary'
import { dashboardBreakdownParams, dashboardChartParams, dashboardTrendParams, useDashboardChart, useDashboardOverview, withFreshDashboardQueries } from '@/features/dashboard/data'
import { dashboardOverviewKey, dashboardPeriodLabel, dashboardSearch } from './period'
import { effectiveDays } from '@/features/analytics/search'
import { qk } from '@/lib/query-keys'

const routeApi = getRouteApi('/admin/')

/// 管理总览。
///
/// 实时、资源和待办置顶，其后展示经营与质量指标，下方保留趋势和分布。
export function DashboardPage() {
  const { t } = useTranslation()
  const { days: presetDays = 7, start_date, end_date, trend = 'combined', distribution = 'model', model_rank = 'amount', channel_rank = 'amount' } = routeApi.useSearch()
  const range = start_date && end_date ? { start: start_date, end: end_date } : null
  const days = effectiveDays({ days: presetDays, start_date, end_date })
  const overview = useDashboardOverview(days, range)
  const chart = useDashboardChart(days, range)
  const calendar = overview.isError ? undefined : overview.data?.calendar
  // Use authoritative server metadata, not the browser's possibly different date.
  const chartWindow = chart.isError ? undefined : chart.data?.window
  const start = calendar?.start_date ?? chartWindow?.start_date ?? chartWindow?.start_at?.slice(0, 10)
  const end = calendar?.end_date ?? chartWindow?.end_date ?? chartWindow?.end_at?.slice(0, 10)
  const timezone = calendar?.timezone ?? chartWindow?.timezone
  const today = calendar?.today ?? chartWindow?.today ?? chartWindow?.end_at?.slice(0, 10)
  const period = dashboardPeriodLabel(days, range, t)
  const search = dashboardSearch(days, range)
  const navigate = routeApi.useNavigate()
  const queryClient = useQueryClient()
  const [refreshing, setRefreshing] = useState(false)
  const refresh = useCallback(async () => {
    setRefreshing(true)
    try {
      // 只刷新本页已挂载的查询，避免牵动其他页面的缓存。
      await withFreshDashboardQueries(async () => { await Promise.all([
        qk.statsRealtime, dashboardOverviewKey(days, range), qk.statsInventory,
        qk.statsChannels(days), qk.adminModels, qk.adminPools, qk.reconciliation, qk.diagnose,
        qk.statsTrend(dashboardTrendParams(days, range)), qk.statsTrend(dashboardChartParams(days, range)),
        qk.statsBreakdown(dashboardBreakdownParams(days, 'model', model_rank, range)),
        qk.statsBreakdown(dashboardBreakdownParams(days, 'channel', channel_rank, range)),
      ].map((queryKey) => queryClient.refetchQueries({ queryKey, exact: true, type: 'active' }))) })
    } finally { setRefreshing(false) }
  }, [days, start_date, end_date, model_rank, channel_rank, queryClient])

  // 汇总、排行和诊断也定期更新，避免首页停留后只剩实时数字在变化。
  useEffect(() => {
    const timer = window.setInterval(() => {
      if (document.visibilityState === 'visible') void refresh()
    }, 60_000)
    return () => window.clearInterval(timer)
  }, [refresh])

  return (
    <div data-slot="dashboard-workspace" className="flex min-w-0 flex-col gap-2 lg:grid lg:h-full lg:min-h-0 lg:grid-rows-[auto_auto_auto_auto_minmax(0,1fr)] lg:gap-1.5 lg:overflow-y-auto lg:[@media(min-height:900px)]:gap-3">
      <PageHeader
        icon={LayoutDashboard}
        title={t('admin:overview')}
        meta={<AttentionStatus days={days} />}
        className="lg:flex-wrap lg:items-center lg:gap-x-2 lg:gap-y-0 lg:max-2xl:[&>div:first-child>span]:hidden [&>div:last-child]:max-w-full [&>div:last-child]:shrink"
        action={<>
          <div className="flex min-w-0 max-w-full flex-wrap items-center gap-2">
            {start && end && timezone && <p data-slot="dashboard-calendar" className="basis-full text-xs leading-4 tabular-nums text-muted-foreground sm:basis-auto lg:max-2xl:max-w-52 2xl:whitespace-nowrap">{t('admin:dashboardCalendar', { start, end, timezone })}</p>}
            <Segmented ariaLabel={t('charts:period')} value={range ? 'custom' : String(days)}
              onChange={(value) => void navigate({ search: (prev) => ({ ...prev, days: Number(value), start_date: undefined, end_date: undefined }) })}
              options={[1, 7, 30].map((value) => ({ value: String(value), label: value === 1 ? t('admin:kpiToday') : t('admin:lastDays', { days: value }) }))} />
            <Tooltip content={t('admin:dashboardPeriodHint')}>
              <button type="button" aria-label={t('admin:dashboardScope')} className="flex h-8 w-8 items-center justify-center rounded text-muted-foreground outline-none hover:bg-muted focus-visible:ring-2 focus-visible:ring-primary/40"><CircleHelp className="h-4 w-4" /></button>
            </Tooltip>
          </div>
          {today && <DateRangePicker today={today} value={range} closeOnApply onApply={(value) => void navigate({ search: (prev) => ({ ...prev, days: undefined, start_date: value.start, end_date: value.end }) })} className="min-w-0 max-w-full px-2 py-1 text-xs" />}
          <Button variant="outline" loading={refreshing} onClick={() => void refresh()}>
            {!refreshing && <RefreshCw className="h-4 w-4" />}{t('common:refresh')}
          </Button>
          <Link to="/admin/stats" search={search} className="inline-flex min-h-10 items-center gap-2 rounded-md bg-primary px-3 text-sm font-medium text-primary-foreground shadow-xs outline-none hover:bg-primary/90 focus-visible:ring-2 focus-visible:ring-primary/40 lg:min-h-9">
            {t('admin:dashboardAnalyze')}<ArrowUpRight className="h-4 w-4" />
          </Link>
        </>}
      />
      <RealtimeCard days={days} />
      <section aria-label={t('admin:dashboardSummary')} className="min-w-0 space-y-2">
        <h2 className="text-xs font-medium text-muted-foreground md:sr-only">{t('admin:dashboardMetricLabel', { period, label: t('admin:dashboardSummary') })}</h2>
        <KpiCards days={days} range={range} />
      </section>
      <OperationsSummary days={days} range={range} />
      <div data-slot="dashboard-charts" className="grid min-w-0 grid-cols-1 items-stretch gap-2 lg:min-h-0 lg:grid-cols-[minmax(0,1.1fr)_minmax(0,1fr)] lg:grid-rows-[minmax(0,1fr)] lg:[@media(min-height:900px)]:gap-3">
        <TrendCard days={days} range={range} metric={trend} onMetricChange={(value) => void navigate({ search: (prev) => ({ ...prev, trend: value === 'combined' ? undefined : value }) })} />
        <DistributionSummary days={days} range={range} view={distribution} onViewChange={(view) => void navigate({ resetScroll: false, search: (prev) => ({ ...prev, distribution: view === 'model' ? undefined : view }) })} modelMetric={model_rank} channelMetric={channel_rank} onMetricChange={(by, metric) => void navigate({ resetScroll: false, search: (prev) => ({ ...prev, [`${by}_rank`]: metric === 'amount' ? undefined : metric }) })} />
      </div>
    </div>
  )
}
