import { createFileRoute } from '@tanstack/react-router'
import { AuditPage } from '@/features/audit/AuditPage'

/// 审计页过滤条件在 URL：用户抽屉 / 渠道行可以 `<Link search={{ target }}>` 深链过来，
/// 落地即已过滤；一段"谁改过这条渠道"的记录本身就该是一个可分享的地址。
export interface AuditSearch {
  actor?: string
  action?: string
  target?: string
  hours?: number
}

function str(v: unknown): string | undefined {
  return typeof v === 'string' && v.trim() !== '' ? v.trim() : undefined
}

function posInt(v: unknown): number | undefined {
  const n = typeof v === 'number' ? v : typeof v === 'string' ? Number(v) : Number.NaN
  return Number.isInteger(n) && n > 0 ? n : undefined
}

export const Route = createFileRoute('/admin/audit')({
  validateSearch: (search: Record<string, unknown>): AuditSearch => ({
    actor: str(search.actor),
    action: str(search.action),
    target: str(search.target),
    hours: posInt(search.hours),
  }),
  component: AuditPage,
})
