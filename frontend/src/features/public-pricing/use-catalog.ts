import { useMemo } from 'react'
import { useQuery } from '@tanstack/react-query'
import { apiFetch, getKey } from '@/lib/api'
import { qk } from '@/lib/query-keys'
import { loadCatalog } from './catalog-loader'
import type { Catalog } from './catalog-loader'

interface MyGroups { current: string; data: Array<{ code: string }> }
let nextCatalogScope = 0

// Scope every rendered group surface, including older servers that still return
// a global directory. The backend independently enforces the same access rule.
export function visibleCatalog(catalog: Catalog, allowed?: ReadonlySet<string>): Catalog {
  const groups = catalog.groups.filter((g) => allowed
    ? allowed.has(g.code)
    : g.self_select || g.is_default || (g.is_default === undefined && g.code === 'default'))
  const codes = new Set(groups.map((g) => g.code))
  return {
    groups,
    models: catalog.models.map((model) => ({
      ...model,
      groups: model.groups.filter((code) => codes.has(code)),
      chat_endpoints_by_group: model.chat_endpoints_by_group && Object.fromEntries(
        Object.entries(model.chat_endpoints_by_group).filter(([code]) => codes.has(code)),
      ),
    })),
  }
}

export function useCatalog() {
  const key = getKey()
  // A fresh opaque cache scope per visit/credential change prevents another
  // account's catalog from flashing on screen. Never put API keys in query keys.
  const scope = useMemo(() => String(++nextCatalogScope), [key])
  return useQuery({
    queryKey: qk.catalog(scope),
    gcTime: 0,
    staleTime: 0,
    retry: false,
    queryFn: async () => {
      const [catalog, mine] = await Promise.all([
        loadCatalog(key ?? ''),
        key ? apiFetch<MyGroups>('/api/me/groups', { key }) : Promise.resolve(undefined),
      ])
      return visibleCatalog(catalog, mine ? new Set(mine.data.map((g) => g.code)) : undefined)
    },
  })
}
