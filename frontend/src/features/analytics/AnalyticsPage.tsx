import { useQuery } from '@tanstack/react-query'
import { getRouteApi, useNavigate } from '@tanstack/react-router'
import { BarChart3, GitFork, TrendingUp } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import type { AnalyticsView, FlowMetric } from '@/features/analytics/route-state'
import { ANALYTICS_VIEWS } from '@/features/analytics/route-state'
import type { TrendMetric } from '@/features/analytics/trend-data'
import { PageHeader } from '@/components/ui/page'
import { Tabs } from '@/components/ui/tabs'
import { Tooltip } from '@/components/ui/tooltip'
import { BreakdownView } from '@/features/analytics/BreakdownView'
import { FilterBar } from '@/features/analytics/FilterBar'
import { FlowView } from '@/features/analytics/FlowView'
import { KpiStrip } from '@/features/analytics/KpiStrip'
import { TrendView } from '@/features/analytics/TrendView'
import { cleanSearch } from '@/features/analytics/search'
import { cubeParams, effectiveDays } from '@/lib/cube'
import type { TrendResp } from '@/lib/cube'
import { AnalysisControls } from './AnalysisControls'
import { FreshnessNotice } from '@/components/freshness-notice'
import { DaysPicker } from '@/components/days-picker'
import { apiFetch } from '@/lib/api'
import { qk } from '@/lib/query-keys'

const routeApi = getRouteApi('/admin/stats')
const VIEW_ICON = {
  trend: TrendingUp,
  breakdown: BarChart3,
  flow: GitFork,
} as const
const FLOW_METRIC_SET = new Set<string>(['amount', 'requests', 'tokens'])

/// 用量分析：一条过滤 + 一排 KPI 仪表带 + 三种工作区视图（趋势 / 拆分 / 流向）。
///
/// 趋势查询由页面持有：KPI 条、页头窗口与趋势视图共用同一份响应（total / previous / data），
/// 过滤芯片的名字回填也来自它的 `scope`——一次请求喂三处。
export function AnalyticsPage() {
  const { t, i18n } = useTranslation()
  const search = routeApi.useSearch()
  const navigate = useNavigate({ from: '/admin/stats' })
  const days = effectiveDays(search)
  const view: AnalyticsView = search.view ?? 'trend'

  const trendParams = cubeParams(search, { stack: view === 'trend' ? search.stack : undefined, metric: search.measure ?? 'amount' })
  const trend = useQuery({
    queryKey: qk.statsTrend(trendParams),
    queryFn: () => apiFetch<TrendResp>(`/admin/stats/trend?${trendParams}`),
    retry: false,
  })

  const viewLabel: Record<AnalyticsView, string> = {
    trend: t('analytics:viewTrend'),
    breakdown: t('analytics:viewBreakdown'),
    flow: t('analytics:viewFlow'),
  }

  // 点击 KPI 格直达对应指标：趋势视图切纵轴；拆分/流向点消费/请求/Token 切度量，点质量指标自动切回趋势图。
  const activeKpiMetric: TrendMetric =
    view === 'trend' ? (search.measure ?? 'amount') : (search.metric ?? 'amount')
  const pickKpiMetric = (metric: TrendMetric) => {
    if (view !== 'trend' && FLOW_METRIC_SET.has(metric)) {
      void navigate({ resetScroll: false, search: (prev) => cleanSearch({ ...prev, metric: metric as FlowMetric }) })
    } else {
      void navigate({ resetScroll: false, search: (prev) => cleanSearch({ ...prev, view: 'trend', measure: metric }) })
    }
  }

  const win = trend.isError ? undefined : trend.data?.window
  const freshness = win?.freshness
  const fmtFreshTime = (v: string | null) => (v ? new Date(v).toLocaleString(i18n.language) : t('analysis:notCollected'))

  // 数据新鲜度健康时收拢为页头内嵌的静默状态提示，不占一整行横幅；仅在有入库延迟时展开醒目告警条。
  const headerMeta = win ? (
    <span className="inline-flex flex-wrap items-center gap-x-2.5 gap-y-1 text-xs text-muted-foreground tabular-nums">
      <span>
        {win.start_date ?? win.start_at} — {win.end_date ?? win.end_at}
        {win.timezone ? ` · ${win.timezone}` : ''}
      </span>
      {freshness && !freshness.stale && (
        <>
          <span aria-hidden className="text-border">·</span>
          <Tooltip
            content={
              <div className="space-y-1 text-left text-[11.5px] tabular-nums">
                <div className="flex items-center gap-1.5 border-b border-border/60 pb-1 text-[11px] font-medium text-muted-foreground">
                  <span aria-hidden className="h-1.5 w-1.5 rounded-full bg-success/80" />
                  <span>{t('analysis:siteFreshness')}</span>
                </div>
                <p className="text-foreground/90">{t('analysis:lastEvent', { time: fmtFreshTime(freshness.last_event_at) })}</p>
                <p className="text-muted-foreground">{t('analysis:lastIngested', { time: fmtFreshTime(freshness.last_ingested_at) })}</p>
              </div>
            }
          >
            <span role="status" className="inline-flex cursor-default items-center gap-1.5 text-muted-foreground/90">
              <span aria-hidden className="h-1.5 w-1.5 rounded-full bg-success/80" />
              <span>{t('analysis:lastEvent', { time: fmtFreshTime(freshness.last_event_at) })}</span>
            </span>
          </Tooltip>
        </>
      )}
    </span>
  ) : undefined

  const workspaceTabs = (
    <Tabs
      variant="underline"
      className="w-auto [&_[role=tablist]]:border-b-0"
      items={ANALYTICS_VIEWS.map((id) => ({ id, label: viewLabel[id], icon: VIEW_ICON[id] }))}
      active={view}
      onChange={(id) =>
        void navigate({ search: (prev) => cleanSearch({ ...prev, view: id as AnalyticsView }) })
      }
    />
  )

  return (
    <div className="flex min-w-0 flex-col gap-3.5">
      <PageHeader
        title={t('analytics:title')}
        meta={headerMeta}
        action={
          <DaysPicker
            days={search.start_date ? 0 : days}
            onPick={(d) => void navigate({ search: (prev) => cleanSearch({ ...prev, days: d, start_date: undefined, end_date: undefined, granularity: undefined }) })}
          />
        }
      />
      {freshness?.stale && <FreshnessNotice value={freshness} />}
      <FilterBar
        search={search}
        scope={trend.data?.scope}
        trailing={
          <AnalysisControls
            value={search}
            today={win?.today}
            onApply={(next) => void navigate({ search: cleanSearch(next) })}
          />
        }
      />
      <KpiStrip
        total={trend.isError ? undefined : trend.data?.total}
        previous={trend.isError ? undefined : trend.data?.previous}
        days={days}
        loading={trend.isLoading}
        activeMetric={trend.isError ? undefined : activeKpiMetric}
        onPickMetric={trend.isError ? undefined : pickKpiMetric}
      />
      {view === 'trend' && (
        <TrendView
          tabs={workspaceTabs}
          search={search}
          resp={trend.data}
          isLoading={trend.isLoading}
          error={trend.error}
          onRetry={() => void trend.refetch()}
        />
      )}
      {view === 'breakdown' && <BreakdownView tabs={workspaceTabs} search={search} />}
      {view === 'flow' && <FlowView tabs={workspaceTabs} search={search} />}
    </div>
  )
}
