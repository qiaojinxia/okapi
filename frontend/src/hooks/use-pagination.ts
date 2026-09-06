import { useNavigate, useSearch } from '@tanstack/react-router'
import { useCallback } from 'react'
import { posInt } from '@/lib/search-params'

/// 管理面表格的每页条数档位；首档即缺省页宽。
export const PAGE_SIZES: readonly number[] = [20, 50, 100]

/// 每页条数硬上限，与后端 `MAX_PAGE` 一致（超了后端也会钳回来，这里先挡掉乱写的地址）。
const MAX_PAGE_SIZE = 200

/// 列表页公用的分页 search params：`page` 1 起算，`limit` 每页条数（与接口参数同名）。
/// 第一页 / 缺省页宽不写进地址，`/admin/users` 与 `/admin/users?page=1` 是同一页。
export interface PageSearch {
  page?: number
  limit?: number
}

/// 各列表路由 `validateSearch` 用：没有别的参数直接 `validateSearch: pageSearch`，
/// 有过滤器的写 `{ ...pageSearch(s), q: text(s.q) }`。
export function pageSearch(search: Record<string, unknown>): PageSearch {
  const page = posInt(search.page)
  const limit = posInt(search.limit)
  return {
    page: page !== undefined && page > 1 ? Math.min(page, 1_000_000) : undefined,
    limit: limit !== undefined && limit <= MAX_PAGE_SIZE ? limit : undefined,
  }
}

export interface UsePaginationOptions {
  /// 缺省每页条数；不传取 `pageSizes` 首档。
  limit?: number
  /// 每页条数可选档位；传 `[]` 则不显示切换器。
  pageSizes?: readonly number[]
}

/// 分页状态。字段名与 `<Pagination>` 的 props 对齐，直接 `{...pager}` 展开即可。
export interface Pager {
  offset: number
  limit: number
  pageSizes: readonly number[]
  onOffset: (offset: number) => void
  onLimit: (limit: number) => void
}

/// 越界的 offset 拉回最后一页起点：删掉末页最后一条、或筛选把结果收窄之后，
/// 停在空页上等用户自己往前翻是不对的。total 为 0 时回到 0。
export function clampOffset(offset: number, limit: number, total: number): number {
  if (offset <= 0 || total <= 0) return 0
  if (offset < total) return offset
  return Math.floor((total - 1) / limit) * limit
}

/// 一页列表的分页状态（offset / 每页条数），驻留在当前路由的 search 里：
/// 刷新、贴给同事、从抽屉深链出去再后退，都回到原来那一页。列表一律服务端分页。
///
/// 翻页与换页宽用 `replace`——翻页像滚动，不该在历史里留一串 `page=2, 3, 4`；
/// 过滤器变化由各页自己 `navigate` 提交，同一次导航里把 `page` 一起清掉，
/// 数据请求只按"新过滤器 + 第一页"发一次，不会先按旧页码空跑一趟。
///
/// 这个 hook 不绑定某条路由（`strict: false`）：各列表路由的 `validateSearch` 已经用
/// `pageSearch` 校过，这里再过一遍只是对没声明分页参数的路由做防御。
export function usePagination({
  limit: initialLimit,
  pageSizes = PAGE_SIZES,
}: UsePaginationOptions = {}): Pager {
  const raw = useSearch({ strict: false }) as Record<string, unknown>
  const { page = 1, limit: urlLimit } = pageSearch(raw)
  const navigate = useNavigate()
  const fallback = initialLimit ?? pageSizes[0] ?? 20
  // 地址里的页宽不在本页档位内（手改的、或别的页面带过来的）就当没写
  const limit = urlLimit !== undefined && pageSizes.includes(urlLimit) ? urlLimit : fallback
  const offset = (page - 1) * limit

  const patch = useCallback(
    (next: PageSearch) =>
      void navigate({
        to: '.',
        search: (prev) => ({ ...prev, ...next }),
        replace: true,
        resetScroll: false,
      }),
    [navigate],
  )
  const onOffset = useCallback(
    (next: number) => {
      const target = Math.floor(Math.max(0, next) / limit) + 1
      patch({ page: target > 1 ? target : undefined })
    },
    [limit, patch],
  )
  // 换页宽时对齐到新页宽：正在看的第一行留在视野内，而不是被甩回第一页
  const onLimit = useCallback(
    (next: number) => {
      const target = Math.floor(offset / next) + 1
      patch({ limit: next === fallback ? undefined : next, page: target > 1 ? target : undefined })
    },
    [offset, fallback, patch],
  )

  return { offset, limit, pageSizes, onOffset, onLimit }
}
