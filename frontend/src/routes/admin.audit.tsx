import { createFileRoute } from '@tanstack/react-router'
import { AuditPage } from '@/features/audit/AuditPage'
import { posInt, text } from '@/lib/search-params'

/// 审计页过滤条件在 URL：用户抽屉 / 渠道行可以 `<Link search={{ target }}>` 深链过来，
/// 落地即已过滤；一段"谁改过这条渠道"的记录本身就该是一个可分享的地址。
export interface AuditSearch {
  actor?: string
  action?: string
  target?: string
  hours?: number
}

export const Route = createFileRoute('/admin/audit')({
  validateSearch: (search: Record<string, unknown>): AuditSearch => ({
    actor: text(search.actor),
    action: text(search.action),
    target: text(search.target),
    hours: posInt(search.hours),
  }),
  component: AuditPage,
})
