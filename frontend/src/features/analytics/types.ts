/// 立方体端点共用的一组度量（比率全部基点，金额 micro-USD）。
export interface CubeMetrics {
  requests: number
  errors: number
  error_rate_bp: number
  prompt_tokens: number
  cached_tokens: number
  completion_tokens: number
  reasoning_tokens: number
  tokens: number
  cache_hit_bp: number
  amount_micro: number
  discount_micro: number
  upstream_cost_micro: number
  avg_latency_ms: number
  avg_ttft_ms: number
}

/// 过滤条件的名字回填（实体已删时名字为 null，芯片退回显示 id）。
export interface ScopeEcho {
  user?: { id: number; username: string | null }
  api_key?: { id: number; name: string | null; key_prefix: string | null; user_id: number | null }
  channel?: { id: number; name: string | null; provider: string | null }
  model?: string
  group?: { code: string; group_ratio: string | null }
}

export interface TrendBucket extends CubeMetrics {
  bucket: string
}

export interface StackedBucket {
  bucket: string
  values: Record<string, { requests: number; amount_micro: number; errors: number; tokens: number }>
}

export interface TrendResp {
  days: number
  granularity: 'hour' | 'day'
  scope: ScopeEcho
  total: Partial<CubeMetrics>
  previous: Partial<CubeMetrics>
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
  previous_amount_micro: number
  /// 环比（基点，可负）；上期为 0 时 null
  delta_bp: number | null
  share_bp: number
  request_share_bp: number
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
  data: BreakdownRow[]
}

export interface FlowNode {
  id: string
  stage: 'user' | 'api_key' | 'group' | 'model' | 'channel'
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
