// 用量立方体（/admin/stats/{trend,breakdown,flow,inventory,entity-usage,…}）的查询客户端：响应类型、URL 筛选解析、
// 查询参数。分析页、首页、质量页、渠道健康时间线、用户 / 密钥列表的用量列都从这里取，不再引用 analytics 这个 feature。
import { oneOf } from '@/lib/search-params'

/// 立方体端点共用的一组度量（比率全部基点，金额 micro-USD）。
export interface Freshness { last_event_at: string | null; last_ingested_at: string | null; pending_events: number; failed_events: number; queue_age_seconds: number | null; event_gap_seconds: number | null; stale: boolean; checked_at: string }
export interface CubeMetrics {
  token_usage_basis?: 'settled'
  token_provenance?: {
    observed_requests: number
    history_complete: boolean
    history_coverage_bp: number | null
    prompt: Record<TokenSource, TokenSourceCount>
    completion: Record<TokenSource, TokenSourceCount>
  }
  suspected_test_requests?: number
  suspected_test_tokens?: number
  test_detection_basis?: 'fixture_model_name'
  recorded_cache_write_tokens?: number | null
  cache_write_known_requests?: number
  measured_cache_hit_bp?: number | null
  measured_cache_hit_requests?: number
  measured_cache_hit_coverage_bp?: number | null
  measured_prompt_tokens?: number | null
  measured_cache_read_tokens?: number | null
  token_detail_observations?: { reasoning_tokens?: { observed_tokens: number | null; observed_records: number; complete: boolean } }
  cost_known_requests?: number
  cost_coverage_bp?: number | null
  known_margin_micro?: number | null
  margin_micro?: number | null
  known_amount_micro?: number
  known_cost_micro?: number
  cache_write_tokens?: number | null
  avg_output_tps_milli?: number | null
  observed_output_tps_milli?: number | null
  output_tps_samples?: number
  ttft_samples?: number
  requests: number
  errors: number
  error_rate_bp: number
  prompt_tokens: number
  cached_tokens: number
  completion_tokens: number
  reasoning_tokens: number
  tokens: number
  cache_hit_bp: number | null
  cache_read_known_requests?: number
  amount_micro: number
  discount_micro: number
  upstream_cost_micro: number
  avg_latency_ms: number | null
  avg_ttft_ms: number | null
}

export type TokenSource = 'upstream' | 'estimated' | 'local_override' | 'unknown'
export interface TokenSourceCount {
  requests: number
  tokens: number | null
  request_share_bp?: number | null
  token_share_bp?: number | null
}

/// 过滤条件的名字回填（实体已删时名字为 null，芯片退回显示 id）。
export interface ScopeEcho {
  user?: { id: number; username: string | null }
  api_key?: { id: number; name: string | null; key_prefix: string | null; user_id: number | null; username?: string | null }
  channel?: { id: number; name: string | null; provider: string | null }
  model?: string
  group?: { code: string; group_ratio: string | null }
}

export interface TrendBucket extends CubeMetrics {
  bucket: string
}

export interface StackedBucket {
  bucket: string
  values: Record<string, CubeMetrics>
}

export interface TrendResp {
  window?: { start_at: string; end_at: string; timezone: string; generated_at: string; today?: string; start_date?: string; end_date?: string; freshness?: Freshness }
  days: number
  granularity: 'hour' | 'day'
  scope: ScopeEcho
  total: Partial<CubeMetrics>
  previous: Partial<CubeMetrics>
  /// `core`：精简查询（只含请求 / Token / 金额 / 错误），不含任何测量口径，也没有上期；`all`：完整查询。
  fields?: 'core' | 'all'
  /// 未堆叠：逐桶度量；堆叠：`series` + 逐桶按序列的值
  data: TrendBucket[] | StackedBucket[]
  stack?: string
  series?: { key: string; label: string | null }[]
}

export interface BreakdownRow extends CubeMetrics {
  key: string
  label: string | null
  rank: number
  previous_rank: number | null
  /// 没查上一窗口（compare=false / fields=core）时为 null——未知，不是 0。
  previous_amount_micro: number | null
  /// 环比（基点，可负）；上期为 0 时 null
  delta_bp: number | null
  share_bp: number
  request_share_bp: number
  token_share_bp?: number
  user_id?: number | null
  username?: string | null
  api_key_id?: number
  key_prefix?: string | null
  channel_id?: number
  provider?: string | null
  channels?: number
  group_ratio?: string | null
}

export interface BreakdownResp {
  days: number
  by: string
  scope: ScopeEcho
  total_amount_micro: number
  total_requests: number
  total_tokens?: number
  /// `core`：精简查询；行内没有延迟、缓存等测量口径，上期名次与环比恒为 null。
  fields?: 'core' | 'all'
  data: BreakdownRow[]
}

export interface FlowNode {
  id: string
  stage: 'user' | 'node' | 'api_key' | 'group' | 'model' | 'channel'
  entity_status?: 'active' | 'deleted' | 'missing' | 'unassigned' | null
  owner_name?: string | null
  key_prefix?: string | null
  provider?: string | null
  key: string
  label: string | null
  other: boolean
  value: number
}

export interface FlowLink {
  source: string
  target: string
  value: number
}

export interface FlowResp {
  days: number
  metric: 'amount' | 'requests' | 'tokens'
  scope: ScopeEcho
  stages: string[]
  total: number
  coverage_bp: number
  truncated: boolean
  nodes: FlowNode[]
  links: FlowLink[]
}

export interface InventoryResp {
  users: { total: number; active: number; new_today: number; new_7d: number }
  api_keys: { total: number; active: number; used_7d: number }
  channels: { total: number; healthy: number; no_key: number; auto_disabled: number; disabled: number }
  channel_keys: {
    active: number
    cooling: number
    rate_limited: number
    quota_exhausted: number
    banned: number
    invalid: number
  }
  models: { total: number; priced: number; served: number }
  groups: number
}

export interface EntityUsage {
  today_micro: number
  window_micro: number
  requests: number
  last_day: string | null
}

export interface EntityUsageResp {
  days: number
  data: Record<string, EntityUsage>
}

export interface TimelinePoint {
  bucket: string
  requests: number
  errors: number
  error_rate_bp: number
  ttft_p50_ms: number
  ttft_p95_ms: number
  failovers: number
  tokens_per_1k_sec: number
}

export interface TimelineResp {
  channel_id: number
  hours: number
  requests: number
  errors: number
  error_rate_bp: number
  data: TimelinePoint[]
}

export const ADVANCED_KEYS = ['start_date', 'end_date', 'granularity', 'model_source', 'endpoint', 'upstream_endpoint', 'node', 'request_type', 'billing_type', 'stream', 'models', 'groups'] as const

export interface AdvancedSearch {
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
}

function str(value: unknown): string | undefined {
  return typeof value === 'string' && value.trim() !== '' ? value.trim() : undefined
}

function choices(value: unknown): string[] | undefined {
  if (typeof value === 'string') { try { value = JSON.parse(value) } catch { return undefined } }
  return Array.isArray(value) && value.length <= 8 && value.every((v) => typeof v === 'string' && v.length > 0 && v.length <= 256) ? value : undefined
}

// 用量与质量趋势共用 URL 解析；空值显式清除，重置不会残留旧筛选。
export function advancedSearch(search: Record<string, unknown>): AdvancedSearch {
  return {
    start_date: str(search.start_date), end_date: str(search.end_date),
    granularity: oneOf(search.granularity, ['hour', 'day']),
    model_source: oneOf(search.model_source, ['billed', 'requested', 'upstream']),
    endpoint: str(search.endpoint), upstream_endpoint: str(search.upstream_endpoint), node: str(search.node),
    request_type: str(search.request_type), billing_type: str(search.billing_type),
    stream: search.stream === true || search.stream === 'true' ? true : search.stream === false || search.stream === 'false' ? false : undefined,
    models: choices(search.models), groups: choices(search.groups),
  }
}

/// 立方体端点的查询条件：过滤维度 + 时间窗 + 高级筛选（分析页的 URL 状态在此之上再加视图参数）。
export interface CubeQuery extends AdvancedSearch {
  user_id?: number
  api_key_id?: number
  channel_id?: number
  model?: string
  group?: string
  days?: number
}

export const DEFAULT_DAYS = 7

export function effectiveDays(s: CubeQuery): number {
  if (s.start_date && s.end_date) {
    const days = Math.round((Date.parse(`${s.end_date}T00:00:00Z`) - Date.parse(`${s.start_date}T00:00:00Z`)) / 86400_000) + 1
    if (days > 0 && days <= 366) return days
  }
  return s.days ?? DEFAULT_DAYS
}

/// 查询条件 → 立方体端点的 query string（只带过滤维度与时间窗；视图参数另加）。
export function cubeParams(s: CubeQuery, extra?: Record<string, string | undefined>): string {
  const p = new URLSearchParams()
  p.set('days', String(effectiveDays(s)))
  if (s.user_id !== undefined) p.set('user_id', String(s.user_id))
  if (s.api_key_id !== undefined) p.set('api_key_id', String(s.api_key_id))
  if (s.channel_id !== undefined) p.set('channel_id', String(s.channel_id))
  if (s.model !== undefined) p.set('model', s.model)
  if (s.group !== undefined) p.set('group', s.group)
  for (const key of ['start_date', 'end_date', 'granularity', 'model_source', 'endpoint', 'upstream_endpoint', 'node', 'request_type', 'billing_type', 'stream'] as const) {
    if (s[key] !== undefined) p.set(key, String(s[key]))
  }
  for (const key of ['models', 'groups'] as const) { if (s[key]?.length) p.set(key, JSON.stringify(s[key])) }
  for (const [k, v] of Object.entries(extra ?? {})) {
    if (v !== undefined && v !== '') p.set(k, v)
  }
  return p.toString()
}
