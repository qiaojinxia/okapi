import { keepPreviousData, useQuery } from '@tanstack/react-query'
import type { ScopeEcho } from '@/lib/cube'
import type { Pager } from '@/hooks/use-pagination'
import { apiFetch } from '@/lib/api'
import { qk } from '@/lib/query-keys'
import type { LogRow } from './types'
import { toParams } from './search'
import type { Draft } from './search'

export function useAdminLogs(applied: Draft, pager: Pager) {
  const params = toParams(applied, pager.offset, pager.limit)
  return useQuery({
    queryKey: qk.adminLogs(params),
    queryFn: () => apiFetch<{ data: LogRow[]; scope?: ScopeEcho }>(`/admin/logs?${params}`),
    // 翻页时保留上一页数据，避免表格闪空
    placeholderData: keepPreviousData,
    retry: false,
  })
}
