import { keepPreviousData, useQuery } from '@tanstack/react-query'
import type { QueryKey } from '@tanstack/react-query'
import { usePagination } from '@/hooks/use-pagination'
import type { UsePaginationOptions } from '@/hooks/use-pagination'
import type { Page } from '@/lib/query-keys'

/// 列表接口的一页。
export interface Paged<T> {
  data: T[]
  total: number
}

/// 接口路径接上分页参数，再接筛选：`pagedPath('/admin/users', page, { q })` → `/admin/users?limit=10&offset=0&q=…`。
/// 值为空串 / null / undefined 的筛选不带。
export function pagedPath(path: string, page: Page, filters?: Record<string, string | number | boolean | null | undefined>): string {
  const params = new URLSearchParams({ limit: String(page.limit), offset: String(page.offset) })
  for (const [key, value] of Object.entries(filters ?? {})) {
    if (value !== undefined && value !== null && value !== '') params.set(key, String(value))
  }
  return `${path}?${params}`
}

/// 服务端分页列表：页码 / 页宽在地址里（usePagination），按页取数；翻页时保留上一页数据，表格不闪成骨架屏。
/// `queryKey` / `queryFn` 都按当前页给出，其余选项原样交给 useQuery。
export function usePagedQuery<R extends Paged<unknown>>({ queryKey, queryFn, pagination, ...options }: {
  queryKey: (page: Page) => QueryKey
  queryFn: (page: Page) => Promise<R>
  pagination?: UsePaginationOptions
  enabled?: boolean
  retry?: boolean | number
  staleTime?: number
  refetchInterval?: number
  refetchIntervalInBackground?: boolean
}) {
  const pager = usePagination(pagination)
  const page: Page = { offset: pager.offset, limit: pager.limit }
  const query = useQuery({ ...options, queryKey: queryKey(page), queryFn: () => queryFn(page), placeholderData: keepPreviousData })
  const rows: R['data'] = query.data?.data ?? []
  return { query, pager, rows }
}

/// 改列表筛选时的 search 更新：合入新条件并清掉页码——筛选变了，停在第 N 页没有意义；
/// 同一次导航里清，数据只按「新条件 + 第一页」请求一次，不会先按旧页码空跑一趟。
/// 用法：`navigate({ search: withFilters({ q: text(value) }) })`。
export function withFilters<S extends { page?: number }>(patch: Partial<NoInfer<S>>) {
  return (prev: S): S => ({ ...prev, ...patch, page: undefined })
}
