export interface OverviewBucket {
  requests: number
  tokens: number
  amount_micro: number
  original_micro: number
  discount_micro: number
  upstream_cost_micro: number
  margin_micro: number | null
  margin_rate_bp: number | null
  errors: number
  error_rate_bp: number
  active_users: number
}

export interface OverviewResp {
  days: number
  today: OverviewBucket
  /// 昨日全天（环比锚点；后端按 mv_user_day 整日聚合，非"昨日同一时刻"）。
  yesterday: OverviewBucket
  window: OverviewBucket
}

export interface MarginDay {
  day: string
  requests: number
  amount_micro: number
  discount_micro: number
}

export interface MarginResp {
  window?: { start_date: string; end_date: string; timezone: string }
  days: number
  data: MarginDay[]
}
