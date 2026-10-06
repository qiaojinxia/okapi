import type { TrendMetric } from './trend-data'
import type { AdvancedSearch } from './advanced-search'

export const ANALYTICS_VIEWS = ['trend', 'breakdown', 'flow'] as const
export type AnalyticsView = (typeof ANALYTICS_VIEWS)[number]

export const BREAKDOWN_DIMS = ['model', 'channel', 'provider', 'user', 'api_key', 'group', 'requested_model', 'upstream_model', 'endpoint', 'upstream_endpoint', 'node', 'request_type', 'billing_type'] as const
export type BreakdownDim = (typeof BREAKDOWN_DIMS)[number]

export const STACK_DIMS = ['model', 'model_group', 'channel', 'group', 'user', 'api_key', 'node', 'endpoint', 'request_type', 'billing_type'] as const
export type StackDim = (typeof STACK_DIMS)[number]

export const FLOW_METRICS = ['amount', 'requests', 'tokens'] as const
export type FlowMetric = (typeof FLOW_METRICS)[number]

/// 用量分析页的全部状态都在 URL：过滤维度 + 时间窗 + 当前视图 + 视图参数。
///
/// 与日志页同一理由，且这页更需要：用户抽屉 / 渠道行 / 模型行都可以
/// `<Link search={{ channel_id }}>` 深链过来落地即已过滤；拆分表里点一行"聚焦"
/// 就是改一次 search——下钻路径（模型 → 哪些渠道 → 哪些用户）天然可前进后退。
export interface AnalyticsSearch extends AdvancedSearch {
  user_id?: number
  api_key_id?: number
  channel_id?: number
  model?: string
  group?: string
  days?: number
  view?: AnalyticsView
  by?: BreakdownDim
  stack?: StackDim
  measure?: TrendMetric
  stages?: string[]
  limit?: number
  metric?: FlowMetric
}

