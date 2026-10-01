import { useQuery } from '@tanstack/react-query'
import { cubeParams } from '@/features/analytics/search'
import type { InventoryResp, TrendResp } from '@/features/analytics/types'
import { apiFetch } from '@/lib/api'
import { qk } from '@/lib/query-keys'
import type { RankingMetric } from './types'

export const DASHBOARD_RANKING_LIMIT = 3
export const DASHBOARD_STALE_TIME = 15_000
export function dashboardBreakdownParams(days: number, by: 'model' | 'channel', metric: RankingMetric = 'amount') {
  const params = new URLSearchParams(cubeParams({ days }, { by, limit: String(DASHBOARD_RANKING_LIMIT), metric: metric === 'amount' ? undefined : metric }))
  params.set('cached', 'true')
  return params.toString()
}

export function dashboardTrendParams(days: number) {
  const params = new URLSearchParams(cubeParams({ days }, { metric: 'amount' }))
  params.set('cached', 'true')
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

// 费用、质量与 Token 构成共用一次汇总查询，保持相同时间窗与数据口径。
export function useDashboardUsage(days: number) {
  const params = dashboardTrendParams(days)
  return useQuery({ queryKey: qk.statsTrend(params), queryFn: () => dashboardFetch<TrendResp>(`/admin/stats/trend?${params}`), staleTime: DASHBOARD_STALE_TIME, retry: false })
}
