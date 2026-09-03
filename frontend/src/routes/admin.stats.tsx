import { createFileRoute } from '@tanstack/react-router'
import { AnalyticsPage } from '@/features/analytics/AnalyticsPage'

export const ANALYTICS_VIEWS = ['trend', 'breakdown', 'flow'] as const
export type AnalyticsView = (typeof ANALYTICS_VIEWS)[number]

export const BREAKDOWN_DIMS = ['model', 'channel', 'provider', 'user', 'api_key', 'group'] as const
export type BreakdownDim = (typeof BREAKDOWN_DIMS)[number]

export const STACK_DIMS = ['model', 'channel', 'group', 'user', 'api_key'] as const
export type StackDim = (typeof STACK_DIMS)[number]

export const FLOW_METRICS = ['amount', 'requests', 'tokens'] as const
export type FlowMetric = (typeof FLOW_METRICS)[number]

/// 用量分析页的全部状态都在 URL：过滤维度 + 时间窗 + 当前视图 + 视图参数。
///
/// 与日志页同一理由，且这页更需要：用户抽屉 / 渠道行 / 模型行都可以
/// `<Link search={{ channel_id }}>` 深链过来落地即已过滤；拆分表里点一行"聚焦"
/// 就是改一次 search——下钻路径（模型 → 哪些渠道 → 哪些用户）天然可前进后退。
export interface AnalyticsSearch {
  user_id?: number
  api_key_id?: number
  channel_id?: number
  model?: string
  group?: string
  days?: number
  view?: AnalyticsView
  by?: BreakdownDim
  stack?: StackDim
  metric?: FlowMetric
}

function str(v: unknown): string | undefined {
  return typeof v === 'string' && v.trim() !== '' ? v.trim() : undefined
}

function posInt(v: unknown): number | undefined {
  const n = typeof v === 'number' ? v : typeof v === 'string' ? Number(v) : Number.NaN
  return Number.isInteger(n) && n > 0 ? n : undefined
}

function oneOf<T extends string>(v: unknown, allowed: readonly T[]): T | undefined {
  return typeof v === 'string' && (allowed as readonly string[]).includes(v) ? (v as T) : undefined
}

export const Route = createFileRoute('/admin/stats')({
  validateSearch: (search: Record<string, unknown>): AnalyticsSearch => ({
    user_id: posInt(search.user_id),
    api_key_id: posInt(search.api_key_id),
    channel_id: posInt(search.channel_id),
    model: str(search.model),
    group: str(search.group),
    days: posInt(search.days),
    view: oneOf(search.view, ANALYTICS_VIEWS),
    by: oneOf(search.by, BREAKDOWN_DIMS),
    stack: oneOf(search.stack, STACK_DIMS),
    metric: oneOf(search.metric, FLOW_METRICS),
  }),
  component: AnalyticsPage,
})
