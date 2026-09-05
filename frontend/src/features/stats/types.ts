/// 错误率红绿灯阈值（基点）：<1% 绿、<5% 黄、其余红。
export const WARN_BP = 100


export const BAD_BP = 500



export interface ChannelRow {
  channel_id: number
  name: string
  provider: string
  requests: number
  errors: number
  error_rate_bp: number
  ttft_p50_ms: number
  ttft_p95_ms: number
  ttft_p99_ms: number
  failovers: number
  sticky_rate_bp: number
  tokens_per_1k_sec: number
  amount_micro: number
}



export interface ModelRow {
  model: string
  requests: number
  tokens: number
  amount_micro: number
  ttft_p50_ms: number
  ttft_p95_ms: number
  ttft_p99_ms: number
  latency_p50_ms: number
  latency_p95_ms: number
  latency_p99_ms: number
  tokens_per_1k_sec: number
}



interface CostCoverage {
  cost_known_requests?: number
  cost_coverage_bp?: number | null
  known_cost_micro?: number
  known_margin_micro?: number | null
}
export interface MarginDay extends CostCoverage {
  day: string
  requests: number
  amount_micro: number
  original_micro: number
  discount_micro: number
  upstream_cost_micro: number
  margin_micro: number | null
}



export interface MarginResp {
  window?: { start_date: string; end_date: string; today: string; timezone: string; generated_at: string; freshness?: import('@/features/analytics/types').Freshness }
  data: MarginDay[]
  total: CostCoverage & {
    requests: number
    errors: number
    error_rate_bp: number
    amount_micro: number
    discount_micro: number
    upstream_cost_micro: number
    margin_micro: number | null
    margin_rate_bp: number | null
  }
}
