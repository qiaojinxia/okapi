import { useMemo } from 'react'
import { useQueries, useQuery } from '@tanstack/react-query'
import { apiFetch, getKey } from '@/lib/api'
import { qk } from '@/lib/query-keys'
import { visibleCatalog } from './use-catalog'
import { catalogParams, loadCatalogModel, loadCatalogStatistics, loadModelPage } from './paged-catalog'
import type { CatalogSearch } from './types'

let nextScope = 0
export function usePagedCatalog(search: CatalogSearch) {
  const key = getKey() ?? ''
  const scope = useMemo(() => `page-${++nextScope}`, [key])
  const base = qk.catalog(scope)
  const mine = useQuery({ queryKey: [...base, 'groups'], retry: false, gcTime: 0,
    queryFn: ({ signal }) => key ? apiFetch<{ data: Array<{ code: string }> }>('/api/me/groups', { key, signal }) : Promise.resolve(null) })
  const allowed = mine.data ? new Set(mine.data.data.map((g) => g.code)) : undefined
  const params = catalogParams(search)
  const page = useQuery({ queryKey: [...base, 'page', params], retry: false, gcTime: 0, enabled: mine.isSuccess,
    queryFn: async ({ signal }) => {
      const result = await loadModelPage(params, key, signal)
      return { ...result, ...visibleCatalog(result, allowed) }
    } })
  const stats = useQuery({ queryKey: [...base, 'statistics'], retry: false, gcTime: 0, enabled: mine.isSuccess,
    queryFn: ({ signal }) => loadCatalogStatistics(key, signal) })
  const ids = [...new Set([...(search.compare?.split(',') ?? []), ...(search.model ? [search.model] : [])])]
  const details = useQueries({ queries: ids.map((id) => ({ queryKey: [...base, 'model', id], retry: false, gcTime: 0, staleTime: 30_000, enabled: mine.isSuccess,
    queryFn: async ({ signal }) => {
      const result = await loadCatalogModel(id, key, signal)
      return visibleCatalog(result, allowed).models[0] ?? null
    } })) })
  const models = new Map(details.flatMap((query, i) => query.data ? [[ids[i], query.data] as const] : []))
  const missing = new Set(ids.filter((_, i) => details[i].isSuccess && details[i].data === null))
  return { page, stats, mine, details, models, missing, detailQuery: details[ids.indexOf(search.model ?? '')] }
}
