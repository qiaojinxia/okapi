import type { LogDiagnostics, LogStats, TokenDetails } from '@/features/logs/types'

// 管理端日志接口（console logs.rs 的 search / stat / refund）的响应类型。

export interface LogRow {
  requested_model?: string
  upstream_model?: string
  endpoint?: string
  upstream_endpoint?: string
  usage_details_recorded?: boolean
  ts: string
  request_id: string
  upstream_request_id: string
  log_type: number
  user_id: number
  username: string
  api_key_id: number
  key_name?: string
  key_prefix?: string
  group: string
  model: string
  channel_id: number
  channel_name: string
  channel_key_id: number
  provider: string
  client_type: string
  client_ip: string
  node: string
  usage: TokenDetails
  amount_micro: number
  original_amount_micro: number
  discount_micro: number
  /// 上游成本（官方价 × 渠道相对成本系数）；成本采集上线前的历史行为 0。
  upstream_cost_micro: number
  status?: number | null
  diagnostics?: LogDiagnostics | null
  request_type?: string
  upstream_cost_known?: boolean | null
  latency_ms: number | null
  ttft_ms: number | null
  is_stream: boolean
  retry_count: number
  failover_count: number
  sticky_layer: number
  upstream_status: number
  error_code: string
  is_error: boolean
  ratio_snapshot: string
}

export interface StatResp extends Partial<LogStats> {
  requests: number
  errors: number
  error_rate_bp: number
  tokens: number
  amount_micro: number
  discount_micro: number
  users: number
  cached_tokens: number
  cache_hit_bp: number | null
  rpm: number
  tpm: number
  rate_source: string
}

export interface RefundResp {
  outcome: 'refunded' | 'already_refunded'
  refunded_micro?: number
  balance_after_micro?: number
}
