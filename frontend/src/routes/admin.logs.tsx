import { createFileRoute } from '@tanstack/react-router'
import { AdminLogsPage } from '@/features/admin-logs/AdminLogsPage'
import { type PageSearch, pageSearch } from '@/hooks/use-pagination'
import { posInt, text } from '@/lib/search-params'

/// 日志页的过滤条件放在 URL 而非组件状态：
/// - 看板任意维度（错误码 / 渠道 / 模型 / 用户）可以直接 `<Link search>` 深链过来，
///   落地即已过滤，而不是"跳过去再把刚看到的东西手敲一遍"；
/// - 刷新不丢、可贴进工单——"这批失败请求"本身就该是一个可分享的地址。
export interface LogSearch extends PageSearch {
  model?: string
  user_id?: number
  api_key_id?: number
  channel_id?: number
  error_code?: string
  request_id?: string
  errors_only?: boolean
  hours?: number
  /// 绝对区间（RFC3339，UTC）；给了 from 就忽略 hours。对账"某一天的账"用。
  from?: string
  to?: string
}

export const Route = createFileRoute('/admin/logs')({
  validateSearch: (search: Record<string, unknown>): LogSearch => ({
    ...pageSearch(search),
    model: text(search.model),
    user_id: posInt(search.user_id),
    api_key_id: posInt(search.api_key_id),
    channel_id: posInt(search.channel_id),
    error_code: text(search.error_code),
    request_id: text(search.request_id),
    errors_only: search.errors_only === true || search.errors_only === 'true' ? true : undefined,
    hours: posInt(search.hours),
    from: text(search.from),
    to: text(search.to),
  }),
  component: AdminLogsPage,
})
