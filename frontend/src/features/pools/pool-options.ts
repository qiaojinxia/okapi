import { queryOptions } from '@tanstack/react-query'
import { apiFetch, ApiError } from '@/lib/api'
import { qk } from '@/lib/query-keys'
import type { PoolRow } from './types'

export interface PoolOptions {
  data: PoolRow[]
  total: number
}

// Selectors need the complete catalog, independently of the pool list's page.
async function loadPoolOptions(): Promise<PoolOptions> {
  const data: PoolRow[] = []
  const codes = new Set<string>()
  let path = '/admin/pools'
  let total: number | undefined
  for (;;) {
    const page = await apiFetch<{ data: PoolRow[]; total?: number }>(path)
    total = page.total ?? total
    if (total !== undefined && (!Number.isSafeInteger(total) || total < 0)) {
      throw new ApiError(502, 'internal_error')
    }
    for (const pool of page.data) {
      if (codes.has(pool.pool_code)) throw new ApiError(502, 'internal_error')
      codes.add(pool.pool_code)
      data.push(pool)
    }
    if (total === undefined || data.length >= total) return { data, total: data.length }
    if (page.data.length === 0) throw new ApiError(502, 'internal_error')
    path = `/admin/pools?limit=200&offset=${data.length}`
  }
}

export const poolOptions = () => queryOptions({
  queryKey: qk.adminPoolOptions,
  queryFn: loadPoolOptions,
  staleTime: 60_000,
})
