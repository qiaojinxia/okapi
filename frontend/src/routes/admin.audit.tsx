import { createFileRoute } from '@tanstack/react-router'
import { AuditPage } from '@/features/audit/AuditPage'
import { type PageSearch, pageSearch } from '@/hooks/use-pagination'
import { posInt, text } from '@/lib/search-params'

/// 审计页过滤条件在 URL：用户抽屉 / 渠道行可以 `<Link search={{ target }}>` 深链过来，
/// 落地即已过滤；一段"谁改过这条渠道"的记录本身就该是一个可分享的地址。页码 / 页宽同样在地址里。
export interface AuditSearch extends PageSearch {
  actor?: string
  action?: string
  target?: string
  ip?: string
  q?: string
  trace_id?: string
  hours?: number
  from?: string
  to?: string
}

export const Route = createFileRoute('/admin/audit')({
  staticData: { fitViewport: true },
  validateSearch: (search: Record<string, unknown>): AuditSearch => ({
    ...pageSearch(search),
    actor: text(search.actor),
    action: text(search.action),
    target: text(search.target),
    ip: text(search.ip),
    q: text(search.q),
    trace_id: text(search.trace_id),
    hours: posInt(search.hours),
    from: text(search.from),
    to: text(search.to),
  }),
  component: AuditPage,
})
