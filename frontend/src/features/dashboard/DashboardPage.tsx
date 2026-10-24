import { useCallback, useEffect, useRef, useState } from 'react'
import { useQueryClient } from '@tanstack/react-query'
import { getRouteApi } from '@tanstack/react-router'
import { useTranslation } from 'react-i18next'
import { AttentionStrip } from '@/features/dashboard/AttentionCard'
import { DashboardHeader } from '@/features/dashboard/DashboardHeader'
import { KpiCards } from '@/features/dashboard/KpiCards'
import { TrendCard } from '@/features/dashboard/TrendCard'
import { OperationsSummary } from '@/features/dashboard/OperationsSummary'
import { DistributionSummary } from '@/features/dashboard/DistributionSummary'
import { dashboardBreakdownParams, dashboardChartParams, dashboardTrendParams, useDashboardChart, useDashboardOverview, withFreshDashboardQueries } from '@/features/dashboard/data'
import { dashboardOverviewKey, dashboardPeriodLabel } from './period'
import { effectiveDays } from '@/features/analytics/search'
import { qk } from '@/lib/query-keys'

const routeApi = getRouteApi('/admin/')
/// 自动刷新时完整用量查询的最短间隔。
const USAGE_AUTO_REFRESH_MS = 5 * 60_000

/// 管理总览。
///
/// 页头一行说清统计日历、实时流量和服务健康；待办在其下成条，其后是经营指标与调用质量，
/// 下方并排趋势和分布。整页自然流动，窄屏逐块折行。
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
  const navigate = routeApi.useNavigate()
  const queryClient = useQueryClient()
  const [refreshing, setRefreshing] = useState(false)
  const usageRefreshedAt = useRef(0)
  const refresh = useCallback(async (auto = false) => {
    if (!auto) setRefreshing(true)
    // 费用、质量与 Token 构成那条完整查询是本页最重的（服务端单条规划就要秒级）：
    // 自动刷新每 5 分钟才带上它一次，手动刷新照旧全部重查
    const withUsage = !auto || Date.now() - usageRefreshedAt.current >= USAGE_AUTO_REFRESH_MS
    if (withUsage) usageRefreshedAt.current = Date.now()
    try {
      // 只刷新本页已挂载的查询，避免牵动其他页面的缓存。
      await withFreshDashboardQueries(async () => { await Promise.all([
        qk.statsRealtime, dashboardOverviewKey(days, range), qk.statsInventory,
        qk.statsChannels(days), qk.adminModels, qk.adminPools, qk.reconciliation, qk.diagnose,
        ...(withUsage ? [qk.statsTrend(dashboardTrendParams(days, range))] : []), qk.statsTrend(dashboardChartParams(days, range)),
        qk.statsBreakdown(dashboardBreakdownParams(days, 'model', model_rank, range)),
        qk.statsBreakdown(dashboardBreakdownParams(days, 'channel', channel_rank, range)),
      ].map((queryKey) => queryClient.refetchQueries({ queryKey, exact: true, type: 'active' }))) })
    } finally { if (!auto) setRefreshing(false) }
  }, [days, start_date, end_date, model_rank, channel_rank, queryClient])

  // 汇总、排行和诊断也定期更新，避免首页停留后只剩实时数字在变化。
  useEffect(() => {
    // 初次加载已经查过完整用量，从现在起算 5 分钟
    usageRefreshedAt.current = Date.now()
    const timer = window.setInterval(() => {
      if (document.visibilityState === 'visible') void refresh(true)
    }, 60_000)
    return () => window.clearInterval(timer)
  }, [refresh])

  return (
    <div data-slot="dashboard-workspace" className="@container flex min-w-0 flex-col gap-2.5">
      <DashboardHeader
        title={t('admin:overview')}
        days={days}
        range={range}
        calendar={start && end && timezone ? { start, end, timezone } : undefined}
        today={today}
        refreshing={refreshing}
        onRefresh={() => void refresh()}
        onPeriod={(value) => void navigate({ search: (prev) => ({ ...prev, days: value, start_date: undefined, end_date: undefined }) })}
        onRange={(value) => void navigate({ search: (prev) => ({ ...prev, days: undefined, start_date: value.start, end_date: value.end }) })}
      />
      <AttentionStrip days={days} />
      <section aria-label={t('admin:dashboardSummary')} className="min-w-0">
        <h2 className="sr-only">{t('admin:dashboardMetricLabel', { period, label: t('admin:dashboardSummary') })}</h2>
        <KpiCards days={days} range={range} />
      </section>
      <OperationsSummary days={days} range={range} />
      <div data-slot="dashboard-charts" className="flex min-w-0 flex-wrap items-stretch gap-2.5">
        <TrendCard days={days} range={range} metric={trend} onMetricChange={(value) => void navigate({ search: (prev) => ({ ...prev, trend: value === 'combined' ? undefined : value }) })} />
        <DistributionSummary days={days} range={range} view={distribution} onViewChange={(view) => void navigate({ resetScroll: false, search: (prev) => ({ ...prev, distribution: view === 'model' ? undefined : view }) })} modelMetric={model_rank} channelMetric={channel_rank} onMetricChange={(by, metric) => void navigate({ resetScroll: false, search: (prev) => ({ ...prev, [`${by}_rank`]: metric === 'amount' ? undefined : metric }) })} />
      </div>
    </div>
  )
}
