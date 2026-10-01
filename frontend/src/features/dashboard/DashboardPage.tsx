import { useCallback, useEffect, useState } from 'react'
import { useQueryClient } from '@tanstack/react-query'
import { getRouteApi, Link } from '@tanstack/react-router'
import { ArrowUpRight, CircleHelp, LayoutDashboard, RefreshCw } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { PageHeader } from '@/components/ui/page'
import { Button } from '@/components/ui/button'
import { Segmented } from '@/components/ui/segmented'
import { Tooltip } from '@/components/ui/tooltip'
import { AttentionStatus } from '@/features/dashboard/AttentionCard'
import { KpiCards } from '@/features/dashboard/KpiCards'
import { RealtimeCard } from '@/features/dashboard/RealtimeCard'
import { TrendCard } from '@/features/dashboard/TrendCard'
import { OperationsSummary } from '@/features/dashboard/OperationsSummary'
import { DistributionSummary } from '@/features/dashboard/DistributionSummary'
import { dashboardBreakdownParams, dashboardChartParams, dashboardTrendParams, withFreshDashboardQueries } from '@/features/dashboard/data'
import { DaysPicker } from '@/features/stats/DaysPicker'
import { qk } from '@/lib/query-keys'

const routeApi = getRouteApi('/admin/')

/// 管理总览。
///
/// 先展示经营与质量指标；资源和待办集中在实时区，下方只保留趋势和分布。
export function DashboardPage() {
  const { t } = useTranslation()
  const { days = 7, scope = 'today', trend = 'combined', model_rank = 'amount', channel_rank = 'amount' } = routeApi.useSearch()
  const navigate = routeApi.useNavigate()
  const queryClient = useQueryClient()
  const [refreshing, setRefreshing] = useState(false)
  const refresh = useCallback(async () => {
    setRefreshing(true)
    try {
      // 只刷新本页已挂载的查询，避免牵动其他页面的缓存。
      await withFreshDashboardQueries(async () => { await Promise.all([
        qk.statsRealtime, qk.statsOverview(days), qk.statsInventory,
        qk.statsChannels(days), qk.adminModels, qk.adminPools, qk.reconciliation, qk.diagnose,
        qk.statsTrend(dashboardTrendParams(days)), qk.statsTrend(dashboardChartParams(days)),
        qk.statsBreakdown(dashboardBreakdownParams(days, 'model', model_rank)),
        qk.statsBreakdown(dashboardBreakdownParams(days, 'channel', channel_rank)),
      ].map((queryKey) => queryClient.refetchQueries({ queryKey, exact: true, type: 'active' }))) })
    } finally { setRefreshing(false) }
  }, [days, model_rank, channel_rank, queryClient])

  // 汇总、排行和诊断也定期更新，避免首页停留后只剩实时数字在变化。
  useEffect(() => {
    const timer = window.setInterval(() => {
      if (document.visibilityState === 'visible') void refresh()
    }, 60_000)
    return () => window.clearInterval(timer)
  }, [refresh])

  return (
    <div className="flex min-w-0 flex-col gap-2">
      <PageHeader
        icon={LayoutDashboard}
        title={t('admin:overview')}
        meta={<AttentionStatus days={days} />}
        className="lg:flex-wrap lg:gap-x-2 lg:max-xl:[&>div:first-child>span]:hidden [&>div:last-child]:max-w-full [&>div:last-child]:shrink"
        action={<>
          <div className="flex min-w-0 max-w-full flex-wrap items-center gap-2">
            <Segmented ariaLabel={t('admin:dashboardScope')} value={scope}
              onChange={(value) => void navigate({ search: (prev) => ({ ...prev, scope: value }) })}
              options={[{ value: 'today', label: t('admin:kpiToday') }, { value: 'window', label: t('admin:dashboardSelectedPeriod') }]} />
            <DaysPicker days={days} onPick={(value) => void navigate({ search: (prev) => ({ ...prev, days: value }) })} />
            <Tooltip content={t(scope === 'today' ? 'admin:dashboardTodayHint' : 'admin:dashboardWindowHint', { days })}>
              <button type="button" aria-label={t('admin:dashboardScope')} className="flex h-8 w-8 items-center justify-center rounded text-muted-foreground outline-none hover:bg-muted focus-visible:ring-2 focus-visible:ring-primary/40"><CircleHelp className="h-4 w-4" /></button>
            </Tooltip>
          </div>
          <Button variant="outline" loading={refreshing} onClick={() => void refresh()}>
            {!refreshing && <RefreshCw className="h-4 w-4" />}{t('common:refresh')}
          </Button>
          <Link to="/admin/stats" search={{ days }} className="inline-flex min-h-10 items-center gap-2 rounded-md bg-primary px-3 text-sm font-medium text-primary-foreground shadow-xs outline-none hover:bg-primary/90 focus-visible:ring-2 focus-visible:ring-primary/40">
            {t('admin:dashboardAnalyze')}<ArrowUpRight className="h-4 w-4" />
          </Link>
        </>}
      />
      <section aria-label={t('admin:dashboardSummary')} className="min-w-0 space-y-2">
        <h2 className="text-xs font-medium text-muted-foreground md:sr-only">{t('admin:dashboardMetricLabel', { period: scope === 'today' ? t('admin:kpiToday') : t('admin:lastDays', { days }), label: t('admin:dashboardSummary') })}</h2>
        <KpiCards days={days} scope={scope} />
      </section>
      <OperationsSummary days={days} />
      <RealtimeCard days={days} />
      <div className="grid min-w-0 grid-cols-1 items-stretch gap-2 lg:grid-cols-[minmax(0,1.1fr)_minmax(0,1fr)]">
        <TrendCard days={days} metric={trend} onMetricChange={(value) => void navigate({ search: (prev) => ({ ...prev, trend: value === 'combined' ? undefined : value }) })} />
        <DistributionSummary days={days} modelMetric={model_rank} channelMetric={channel_rank} onMetricChange={(by, metric) => void navigate({ search: (prev) => ({ ...prev, [`${by}_rank`]: metric === 'amount' ? undefined : metric }) })} />
      </div>
    </div>
  )
}
