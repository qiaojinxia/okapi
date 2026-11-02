import { queryOptions } from '@tanstack/react-query'
import { apiFetch } from '@/lib/api'
import { loadAll } from '@/lib/load-all'
import { qk } from '@/lib/query-keys'
import type { EgressBinding, ProxyGroupRow, ProxyRow, TrafficRange, TrafficResp } from './types'

export const proxyOptions = () =>
  queryOptions({
    queryKey: qk.adminProxyOptions,
    queryFn: () => loadAll<ProxyRow>('/admin/proxies', (p) => p.id),
    staleTime: 30_000,
  })

export const proxyGroupOptions = () =>
  queryOptions({
    queryKey: qk.adminProxyGroupOptions,
    queryFn: () => loadAll<ProxyGroupRow>('/admin/proxy-groups', (g) => g.code),
    staleTime: 30_000,
  })

export const egressDefaultOptions = () =>
  queryOptions({
    queryKey: qk.egressDefault,
    queryFn: () => apiFetch<{ egress: EgressBinding }>('/admin/egress/default'),
    staleTime: 30_000,
  })

/// 流量每分钟落库一次，一分钟刷新足够。
export const proxyTrafficOptions = (range: TrafficRange) =>
  queryOptions({
    queryKey: qk.adminProxyTraffic(range),
    queryFn: () => apiFetch<TrafficResp>(`/admin/proxies/traffic?range=${range}`),
    staleTime: 60_000,
    refetchInterval: 60_000,
  })
