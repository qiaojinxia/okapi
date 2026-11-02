import { queryOptions } from '@tanstack/react-query'
import { loadAll } from '@/lib/load-all'
import { qk } from '@/lib/query-keys'
import type { PoolRow } from './types'

export interface PoolOptions {
  data: PoolRow[]
  total: number
}

// Selectors need the complete catalog, independently of the pool list's page.
async function loadPoolOptions(): Promise<PoolOptions> {
  const data = await loadAll<PoolRow>('/admin/pools', (pool) => pool.pool_code)
  return { data, total: data.length }
}

export const poolOptions = () => queryOptions({
  queryKey: qk.adminPoolOptions,
  queryFn: loadPoolOptions,
  staleTime: 60_000,
})
