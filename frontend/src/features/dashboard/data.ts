import { useQuery } from '@tanstack/react-query'
import { cubeParams } from '@/features/analytics/search'
import type { InventoryResp, TrendResp } from '@/features/analytics/types'
import { apiFetch } from '@/lib/api'
import { qk } from '@/lib/query-keys'
import type { RankingMetric } from './types'

export const DASHBOARD_RANKING_LIMIT = 3
export const dashboardBreakdownParams = (days: number, by: 'model' | 'channel', metric: RankingMetric = 'amount') => cubeParams({ days }, { by, limit: String(DASHBOARD_RANKING_LIMIT), metric: metric === 'amount' ? undefined : metric })

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
  const params = cubeParams({ days }, { metric: 'amount' })
  return useQuery({ queryKey: qk.statsTrend(params), queryFn: () => apiFetch<TrendResp>(`/admin/stats/trend?${params}`), retry: false })
}
