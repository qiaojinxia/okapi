import { queryOptions } from '@tanstack/react-query'
import { apiFetch, ApiError } from '@/lib/api'
import { qk } from '@/lib/query-keys'
import type { EgressBinding, ProxyGroupRow, ProxyRow } from './types'

/// 选择器要的是完整目录，与列表页的分页无关（同 `poolOptions`）。
async function loadAll<T>(base: string, id: (row: T) => string | number): Promise<T[]> {
  const data: T[] = []
  const seen = new Set<string | number>()
  for (;;) {
    const page = await apiFetch<{ data: T[]; total?: number }>(
      `${base}?limit=200&offset=${data.length}`,
    )
    const total = page.total
    if (total !== undefined && (!Number.isSafeInteger(total) || total < 0)) {
      throw new ApiError(502, 'internal_error')
    }
    for (const row of page.data) {
      if (seen.has(id(row))) throw new ApiError(502, 'internal_error')
      seen.add(id(row))
      data.push(row)
    }
    if (total === undefined || data.length >= total) return data
    if (page.data.length === 0) throw new ApiError(502, 'internal_error')
  }
}

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
