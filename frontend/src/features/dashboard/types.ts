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
  calendar?: { start_date: string; end_date: string; today: string; timezone: string; generated_at: string }
  today: OverviewBucket
  /// 昨日全天参考；非昨日同一时刻，不用于今日涨跌计算。
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
export type RankingMetric = 'amount' | 'requests' | 'tokens'
export type DashboardTrend = 'combined' | RankingMetric
export type DistributionView = 'model' | 'channel' | 'tokens'
