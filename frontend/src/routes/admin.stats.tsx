import { createFileRoute } from '@tanstack/react-router'
import { TREND_METRICS, type TrendMetric } from '@/features/analytics/trend-data'
import { AnalyticsPage } from '@/features/analytics/AnalyticsPage'

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
export interface AnalyticsSearch {
  user_id?: number
  api_key_id?: number
  channel_id?: number
  model?: string
  group?: string
  days?: number
  start_date?: string
  end_date?: string
  granularity?: 'hour' | 'day'
  model_source?: 'billed' | 'requested' | 'upstream'
  endpoint?: string
  upstream_endpoint?: string
  node?: string
  request_type?: string
  billing_type?: string
  stream?: boolean
  models?: string[]
  groups?: string[]
  view?: AnalyticsView
  by?: BreakdownDim
  stack?: StackDim
  measure?: TrendMetric
  stages?: string[]
  limit?: number
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

function strings(value: unknown): string[] | undefined {
  if (typeof value === 'string') { try { value = JSON.parse(value) } catch { return undefined } }
  return Array.isArray(value) && value.length <= 8 && value.every((v) => typeof v === 'string' && v.length > 0 && v.length <= 256) ? value : undefined
}

export const Route = createFileRoute('/admin/stats')({
  validateSearch: (search: Record<string, unknown>): AnalyticsSearch => ({
    user_id: posInt(search.user_id),
    api_key_id: posInt(search.api_key_id),
    channel_id: posInt(search.channel_id),
    model: str(search.model),
    group: str(search.group),
    days: posInt(search.days),
    start_date: str(search.start_date), end_date: str(search.end_date),
    granularity: oneOf(search.granularity, ['hour', 'day']),
    model_source: oneOf(search.model_source, ['billed', 'requested', 'upstream']),
    endpoint: str(search.endpoint), upstream_endpoint: str(search.upstream_endpoint), node: str(search.node),
    request_type: str(search.request_type), billing_type: str(search.billing_type),
    stream: search.stream === true || search.stream === 'true' ? true : search.stream === false || search.stream === 'false' ? false : undefined,
    models: strings(search.models), groups: strings(search.groups),
    view: oneOf(search.view, ANALYTICS_VIEWS),
    by: oneOf(search.by, BREAKDOWN_DIMS),
    stack: oneOf(search.stack, STACK_DIMS),
    measure: oneOf(search.measure, TREND_METRICS),
    stages: strings(search.stages), limit: posInt(search.limit),
    metric: oneOf(search.metric, FLOW_METRICS),
  }),
  component: AnalyticsPage,
})
