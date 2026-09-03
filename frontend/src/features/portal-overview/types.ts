export type Scope = 'key' | 'user'

/// /api/me/stats/breakdown 的一行：(day, model) 粒度 + token 四轴。
export interface BreakdownRow {
  day: string
  model: string
  requests: number
  prompt_tokens: number
  cached_tokens: number
  completion_tokens: number
  reasoning_tokens: number
  amount_micro: number
  discount_micro: number
  errors: number
}

export interface BreakdownTotal {
  requests: number
  prompt_tokens: number
  cached_tokens: number
  completion_tokens: number
  reasoning_tokens: number
  tokens: number
  amount_micro: number
  discount_micro: number
  cache_hit_bp: number
  avg_rpm_micro: number
  avg_tpm_micro: number
}

/// 限流器视角的当前速率（key 视角才有；上限未配为 null）。
export interface LiveRate {
  rpm: number
  tpm: number
  rpd: number
  rpm_limit: number | null
  tpm_limit: number | null
  rpd_limit: number | null
}

export interface BreakdownResp {
  scope: Scope
  days: number
  total: BreakdownTotal
  live: LiveRate | null
  /// 钱包级窗口消费（不随 scope 变）：算"余额还能撑几天"用整个钱包的日均。
  wallet_window_spend_micro?: number
  data: BreakdownRow[]
}

/// 余额可用天数（new-api 用户看板 Runway 卡口径：余额 ÷ 近期日均消费）。
/// 无消费 → null（"没有近期用量"而不是"∞ 天"）；余额 ≤ 0 → 0。
export function runwayDays(balanceMicro: number, windowSpendMicro: number, days: number): number | null {
  if (balanceMicro <= 0) return 0
  if (windowSpendMicro <= 0 || days <= 0) return null
  return balanceMicro / (windowSpendMicro / days)
}

/// 按模型合计（三个视图共用的折叠）。
export function sumByModel(rows: BreakdownRow[]): Map<string, BreakdownRow> {
  const out = new Map<string, BreakdownRow>()
  for (const r of rows) {
    const cur = out.get(r.model)
    if (cur === undefined) {
      out.set(r.model, { ...r, day: '' })
      continue
    }
    cur.requests += r.requests
    cur.prompt_tokens += r.prompt_tokens
    cur.cached_tokens += r.cached_tokens
    cur.completion_tokens += r.completion_tokens
    cur.reasoning_tokens += r.reasoning_tokens
    cur.amount_micro += r.amount_micro
    cur.discount_micro += r.discount_micro
    cur.errors += r.errors
  }
  return out
}
