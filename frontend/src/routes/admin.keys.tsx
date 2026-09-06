import { createFileRoute } from '@tanstack/react-router'
import { AdminKeysPage } from '@/features/keys/AdminKeysPage'
import { type PageSearch, pageSearch } from '@/hooks/use-pagination'
import { posInt, text } from '@/lib/search-params'

/// 令牌管理页的检索条件在 URL：用户页 / 排障记录可以 `<Link search={{ user_id }}>` 直达
/// "这个人的所有令牌"。
export interface KeysSearch extends PageSearch {
  /// 关键词（令牌名 / 用户名）。
  q?: string
  user_id?: number
}

export const Route = createFileRoute('/admin/keys')({
  validateSearch: (search: Record<string, unknown>): KeysSearch => ({
    ...pageSearch(search),
    q: text(search.q),
    user_id: posInt(search.user_id),
  }),
  component: AdminKeysPage,
})
