export interface OverviewBucket {
  requests: number
  tokens: number
  amount_micro: number
  original_micro: number
  discount_micro: number
  upstream_cost_micro: number
  margin_micro: number
  margin_rate_bp: number
  errors: number
  error_rate_bp: number
  active_users: number
}

export interface OverviewResp {
  days: number
  today: OverviewBucket
  window: OverviewBucket
}

export interface MarginDay {
  day: string
  requests: number
  amount_micro: number
  discount_micro: number
}

export interface MarginResp {
  days: number
  data: MarginDay[]
}
