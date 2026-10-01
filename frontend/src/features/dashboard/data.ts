import { useQuery } from '@tanstack/react-query'
import { cubeParams } from '@/features/analytics/search'
import type { InventoryResp, TrendResp } from '@/features/analytics/types'
import { apiFetch } from '@/lib/api'
import { qk } from '@/lib/query-keys'
import type { RankingMetric } from './types'

export const DASHBOARD_RANKING_LIMIT = 3
export const DASHBOARD_STALE_TIME = 15_000
// 首页只读排行的金额 / 请求 / Token 与占比，不展示延迟、缓存等测量口径，也不展示环比：
// fields=core 走精简数据源，compare=false 省掉整条上期查询。完整源每条查询要 ~5s 的规划时间。
export function dashboardBreakdownParams(days: number, by: 'model' | 'channel', metric: RankingMetric = 'amount') {
  const params = new URLSearchParams(cubeParams({ days }, { by, limit: String(DASHBOARD_RANKING_LIMIT), metric: metric === 'amount' ? undefined : metric }))
  params.set('cached', 'true')
  params.set('fields', 'core')
  params.set('compare', 'false')
  return params.toString()
}

// 趋势图只读每个桶的请求 / 金额 / Token：精简查询，毫秒级返回，不等质量与 Token 构成。
export function dashboardChartParams(days: number) {
  const params = new URLSearchParams(cubeParams({ days }, { metric: 'amount' }))
  params.set('cached', 'true')
  params.set('fields', 'core')
  return params.toString()
}

// 费用、质量与 Token 构成需要全部测量口径（用量来源、缓存、延迟……）；首页不展示环比，不查上期。
export function dashboardTrendParams(days: number) {
  const params = new URLSearchParams(cubeParams({ days }, { metric: 'amount' }))
  params.set('cached', 'true')
  params.set('compare', 'false')
  return params.toString()
}

let freshQueries = 0
export async function withFreshDashboardQueries(fetch: () => Promise<void>) {
  freshQueries += 1
  try { await fetch() } finally { freshQueries -= 1 }
}

export function dashboardFetch<T>(path: string) {
  return apiFetch<T>(path, { fresh: freshQueries > 0 })
}

export function useDashboardInventory() {
  return useQuery({
    queryKey: qk.statsInventory,
    queryFn: () => apiFetch<InventoryResp>('/admin/stats/inventory'),
    staleTime: 60_000,
    retry: false,
  })
}

export function useDashboardChart(days: number) {
  const params = dashboardChartParams(days)
  return useQuery({ queryKey: qk.statsTrend(params), queryFn: () => dashboardFetch<TrendResp>(`/admin/stats/trend?${params}`), staleTime: DASHBOARD_STALE_TIME, retry: false })
}

// 费用、质量与 Token 构成共用一次完整汇总查询，保持相同时间窗与数据口径。
export function useDashboardUsage(days: number) {
  const params = dashboardTrendParams(days)
  return useQuery({ queryKey: qk.statsTrend(params), queryFn: () => dashboardFetch<TrendResp>(`/admin/stats/trend?${params}`), staleTime: DASHBOARD_STALE_TIME, retry: false })
}
