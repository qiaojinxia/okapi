export interface Snapshot {
  epoch?: number
  base_price_per_1m_usd?: string | number | null
  mode: string
  model_ratio?: string | number | null
  completion_ratio?: string | number | null
  cache_ratio?: string | number | null
  cache_write_ratio?: string | number | null
  audio_ratio?: string | number | null
  audio_completion_ratio?: string | number | null
  image_ratio?: string | number | null
  final_unit_price_input_per_1m_usd?: string | number | null
  per_call_price_usd?: string | number | null
  media_units?: number
  group: string
  group_ratio: string | number
  user_multiplier: string | number
  rules: { code: string; multiplier: string | number }[]
}

export interface LogRow {
  id: number
  request_id: string
  model: string
  requested_model?: string | null
  endpoint?: string | null
  log_type: number
  status: number
  api_key_id: number | null
  key_name: string
  pool?: number
  usage_details_recorded?: boolean
  usage: {
    prompt_tokens: number
    cached_tokens: number
    completion_tokens: number
    reasoning_tokens: number
    cache_read_reported?: boolean | null
    cache_write_reported?: boolean | null
    cache_write_tokens?: number | null
    audio_prompt_tokens?: number | null
    image_prompt_tokens?: number | null
    audio_completion_tokens?: number | null
  }
  amount_micro: number
  net_amount_micro?: number
  original_amount_micro: number
  discount_micro: number
  pricing_snapshot: Snapshot | null
  error_code: string | null
  latency_ms: number | null
  ttft_ms: number | null
  is_stream: boolean
  created_at: string
}

export interface LogsResp { scope: string; data: LogRow[]; next_before: number | null }
export interface LogStats {
  records: number; settled: number; failed: number; refunded: number; pending: number
  amount_micro: number; refunded_amount_micro: number
  prompt_tokens: number; completion_tokens: number; cached_tokens: number; cache_read_samples: number
  avg_latency_ms: number | null; latency_samples: number
  avg_ttft_ms: number | null; ttft_samples: number
}

export const billingStatus = (status: number) => ({ 10: 'pending', 20: 'settled', 30: 'refunded', 40: 'failed' } as const)[status as 10 | 20 | 30 | 40] ?? 'unknownStatus'
export const netAmount = (row: LogRow) => row.net_amount_micro ?? (row.status === 20 ? row.amount_micro : 0)
export const cacheRead = (row: LogRow) => row.usage.cache_read_reported === true || row.usage.cached_tokens > 0 ? row.usage.cached_tokens : null
export const cacheWrite = (row: LogRow) => row.usage.cache_write_reported === true || (row.usage.cache_write_tokens ?? 0) > 0 ? row.usage.cache_write_tokens ?? null : null

// Only a known read with a valid input denominator can express cache coverage.
export function cacheReadShare(row: LogRow, locale: string): string | null {
  const read = cacheRead(row), input = row.usage.prompt_tokens
  if (read === null || !Number.isSafeInteger(read) || !Number.isSafeInteger(input) || read <= 0 || input <= 0 || read > input) return null
  const format = (ratio: number) => ratio.toLocaleString(locale, { style: 'percent', maximumFractionDigits: 1 })
  const share = read / input
  // Do not round a real hit down to zero, or a partial hit up to full coverage.
  return share < 0.001 ? `<${format(0.001)}` : share > 0.999 && share < 1 ? `>${format(0.999)}` : format(share)
}

// Six decimal places preserve the ledger's micro-USD precision, including tiny calls.
export const logMoney = (micro: number, locale: string) => new Intl.NumberFormat(locale, { style: 'currency', currency: 'USD', minimumFractionDigits: 2, maximumFractionDigits: 6 }).format(micro / 1_000_000)
export const duration = (ms: number | null | undefined, locale: string) => ms == null || ms < 0 ? '—' : ms < 1000 ? `${Math.round(ms).toLocaleString(locale)} ms` : `${(ms / 1000).toLocaleString(locale, { maximumFractionDigits: 2 })} s`

// Display-only reference amounts. Never used to charge a request; never read live prices.
// BigInt keeps the snapshot's six decimal places through multiplication.
const scaled = (value: string | number | null | undefined): bigint | null => {
  if (value == null || !/^\d+(\.\d{1,6})?$/.test(String(value))) return null
  const [whole, fraction = ''] = String(value).split('.')
  return BigInt(whole) * 1_000_000n + BigInt(fraction.padEnd(6, '0'))
}
export function billingLines(row: LogRow) {
  const s = row.pricing_snapshot, u = row.usage
  const unit = scaled(s?.final_unit_price_input_per_1m_usd)
  if (!s || unit === null || !row.usage_details_recorded || !['ratio', 'tiered'].includes(s.mode)) return []
  if ([u.cache_write_tokens, u.audio_prompt_tokens, u.image_prompt_tokens, u.audio_completion_tokens].some((n) => n == null || !Number.isSafeInteger(n) || n < 0)) return []
  const normal = u.prompt_tokens - u.cached_tokens - (u.cache_write_tokens ?? 0) - (u.audio_prompt_tokens ?? 0) - (u.image_prompt_tokens ?? 0)
  const textOut = u.completion_tokens - (u.audio_completion_tokens ?? 0)
  if (normal < 0 || textOut < 0) return []
  const audio = scaled(s.audio_ratio), audioOut = scaled(s.audio_completion_ratio)
  const audioOutputRatio = audio !== null && audioOut !== null ? (audio === 1_000_000n && audioOut === 1_000_000n ? scaled(s.completion_ratio) : audio * audioOut / 1_000_000n) : null
  const parts: [string, number | null, bigint | null][] = [
    ['normalInput', normal, 1_000_000n],
    ['cacheRead', cacheRead(row), scaled(s.cache_ratio)],
    ['cacheWrite', cacheWrite(row), scaled(s.cache_write_ratio)],
    ['textOutput', textOut, scaled(s.completion_ratio)],
    ...(u.audio_prompt_tokens ? [['audioInput', u.audio_prompt_tokens, audio] as [string, number, bigint | null]] : []),
    ...(u.image_prompt_tokens ? [['imageInput', u.image_prompt_tokens, scaled(s.image_ratio)] as [string, number, bigint | null]] : []),
    ...(u.audio_completion_tokens ? [['audioOutput', u.audio_completion_tokens, audioOutputRatio] as [string, number, bigint | null]] : []),
  ]
  return parts.map(([name, quantity, ratio]) => ({ name, quantity,
    unitMicro: ratio === null ? null : Number(unit * ratio) / 1_000_000,
    amountMicro: ratio === null || quantity === null ? null : Number(unit * ratio * BigInt(quantity)) / 1_000_000_000_000,
  }))
}
