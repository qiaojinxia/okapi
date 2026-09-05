import { useQuery } from '@tanstack/react-query'
import { getRouteApi, useNavigate } from '@tanstack/react-router'
import { useTranslation } from 'react-i18next'
import type { AnalyticsView } from '@/routes/admin.stats'
import { ANALYTICS_VIEWS } from '@/routes/admin.stats'
import { PageHeader } from '@/components/ui/page'
import { Tabs } from '@/components/ui/tabs'
import { BreakdownView } from '@/features/analytics/BreakdownView'
import { FilterBar } from '@/features/analytics/FilterBar'
import { FlowView } from '@/features/analytics/FlowView'
import { KpiStrip } from '@/features/analytics/KpiStrip'
import { TrendView } from '@/features/analytics/TrendView'
import { cleanSearch, cubeParams, effectiveDays } from '@/features/analytics/search'
import type { TrendResp } from '@/features/analytics/types'
import { AnalysisControls } from './AnalysisControls'
import { FreshnessNotice } from './FreshnessNotice'
import { DaysPicker } from '@/features/stats/DaysPicker'
import { apiFetch } from '@/lib/api'
import { qk } from '@/lib/query-keys'

const routeApi = getRouteApi('/admin/stats')

/// 用量分析：一条过滤 + 一排 KPI + 三种切法（趋势 / 拆分 / 流向）。
///
/// 这页替代旧统计页的"七个页签各查一张 MV"：那七签里"花在哪个模型 / 谁花得多 /
/// 按分组"其实是同一个问题（用量的构成）换了三个维度，而"渠道健康 / 模型时延 /
/// 错误分布"是另一个问题（服务质量），"收入 / 资金流入"又是第三个（经营）。
/// 三个问题三页（洞察分组下的三个入口），本页只答第一个——但答到底：任意维度
/// 过滤下都成立，且能一层层下钻。
///
/// 趋势查询由页面持有：KPI 条与趋势视图共用同一份响应（total / previous / data），
/// 过滤芯片的名字回填也来自它的 `scope`——一次请求喂三处。
export function AnalyticsPage() {
  const { t } = useTranslation()
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

  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        title={t('analytics:title')}
        description={t('analytics:desc')}
        action={
          <DaysPicker
            days={search.start_date ? 0 : days}
            onPick={(d) => void navigate({ search: (prev) => cleanSearch({ ...prev, days: d, start_date: undefined, end_date: undefined, granularity: undefined }) })}
          />
        }
      />
      <FilterBar search={search} scope={trend.data?.scope} />
      <AnalysisControls value={search} today={trend.data?.window?.today} onApply={(next) => void navigate({ search: cleanSearch(next) })} />
      <FreshnessNotice value={trend.data?.window?.freshness} />
      <KpiStrip
        total={trend.data?.total}
        previous={trend.data?.previous}
        days={days}
        loading={trend.isLoading}
      />
      <Tabs
        items={ANALYTICS_VIEWS.map((id) => ({ id, label: viewLabel[id] }))}
        active={view}
        onChange={(id) =>
          void navigate({ search: (prev) => cleanSearch({ ...prev, view: id as AnalyticsView }) })
        }
      />
      {view === 'trend' && (
        <TrendView
          search={search}
          resp={trend.data}
          isLoading={trend.isLoading}
          error={trend.error}
        />
      )}
      {view === 'breakdown' && <BreakdownView search={search} />}
      {view === 'flow' && <FlowView search={search} />}
    </div>
  )
}
