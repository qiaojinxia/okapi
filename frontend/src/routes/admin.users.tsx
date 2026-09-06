import { createFileRoute } from '@tanstack/react-router'
import { UsersPage } from '@/features/users/UsersPage'
import { type PageSearch, pageSearch } from '@/hooks/use-pagination'
import { text } from '@/lib/search-params'

export interface UsersSearch extends PageSearch {
  /// 关键词（用户名 / 邮箱）。
  q?: string
}

export const Route = createFileRoute('/admin/users')({
  validateSearch: (search: Record<string, unknown>): UsersSearch => ({
    ...pageSearch(search),
    q: text(search.q),
  }),
  component: UsersPage,
})
