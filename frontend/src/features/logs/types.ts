export interface Snapshot {
  input_unit?: string | null
  input_characters?: number | null
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
  service_tier?: string | null
  tier_ratio?: string | number | null
  image_completion_tokens?: number | null
  modality_ratios?: Partial<Record<'image_cache_read' | 'audio_cache_read' | 'image_cache_write' | 'audio_cache_write' | 'image_output' | 'cache_write_5m' | 'cache_write_1h', string | number | null>> | null
  final_unit_price_input_per_1m_usd?: string | number | null
  per_call_price_usd?: string | number | null
  media_units?: number
  group: string
  group_ratio: string | number
  user_multiplier: string | number
  rules: { code: string; multiplier: string | number }[]
}

export interface TokenDetails {
  input_unit?: string | null
  input_characters?: number | null
  reported_details?: { reasoning?: boolean | null; prompt?: { audio?: boolean | null; image?: boolean | null }; completion?: { audio?: boolean | null; image?: boolean | null } } | null
  prompt_tokens: number
  cached_tokens: number
  completion_tokens: number
  reasoning_tokens: number
  cache_read_reported?: boolean | null
  cache_write_reported?: boolean | null
  cache_write_tokens?: number | null
  cache_write_5m_tokens?: number | null
  cache_write_1h_tokens?: number | null
  audio_prompt_tokens?: number | null
  image_prompt_tokens?: number | null
  audio_completion_tokens?: number | null
  image_completion_tokens?: number | null
  cache_read_modalities?: { audio_tokens: number; image_tokens: number } | null
  cache_write_modalities?: { audio_tokens: number; image_tokens: number } | null
  prompt_source?: string | null
  completion_source?: string | null
  upstream_usage?: { prompt_tokens: number | null; completion_tokens: number | null } | null
}

export interface LogDiagnostics {
  media?: { image_size?: string; image_quality?: string; requested_images?: number; video_size?: string; requested_video_seconds?: number }
  error_phase?: string
  error_message?: string
  response_model?: string
  reasoning_effort?: string
  user_agent?: string
  session_id?: string
  request_failed?: boolean
  stream_end_reason?: string
  attempts_truncated?: boolean
  attempts?: { channel_id: number; channel_key_id: number; provider?: string; upstream_model?: string; upstream_endpoint?: string; duration_ms?: number; status?: number; outcome?: string; error_code?: string; error_phase?: string; error_message?: string; egress_proxy_id?: number }[]
}

export interface LogRow {
  id: number
  request_id: string
  upstream_request_id?: string | null
  is_error?: boolean
  diagnostics?: LogDiagnostics | null
  model: string
  requested_model?: string | null
  endpoint?: string | null
  log_type: number
  status: number
  api_key_id: number | null
  key_name: string
  pool?: number
  usage_details_recorded?: boolean
  usage: TokenDetails
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
  errors?: number
  records: number; settled: number; failed: number; refunded: number; pending: number
  amount_micro: number; refunded_amount_micro: number
  prompt_tokens: number; completion_tokens: number; cached_tokens: number; cache_read_samples: number
  avg_latency_ms: number | null; latency_samples: number
  avg_ttft_ms: number | null; ttft_samples: number
  cache_write_tokens?: number | null; cache_write_samples?: number
  cache_write_5m_tokens?: number | null; cache_write_1h_tokens?: number | null; cache_write_ttl_samples?: number
  reasoning_tokens?: number | null
  audio_prompt_tokens?: number | null; image_prompt_tokens?: number | null
  audio_completion_tokens?: number | null; image_completion_tokens?: number | null
  audio_prompt_samples?: number; image_prompt_samples?: number
  audio_completion_samples?: number; image_completion_samples?: number
  cache_read_audio_tokens?: number | null; cache_read_image_tokens?: number | null
  cache_write_audio_tokens?: number | null; cache_write_image_tokens?: number | null
  cache_read_modal_samples?: number; cache_write_modal_samples?: number
  measured_cache_hit_bp?: number | null; measured_cache_hit_requests?: number
  token_provenance?: {
    prompt: Record<'upstream' | 'estimated' | 'local_override' | 'unknown', { requests: number; tokens: number | null }>
    completion: Record<'upstream' | 'estimated' | 'local_override' | 'unknown', { requests: number; tokens: number | null }>
  }
}

export const billingStatus = (status: number) => ({ 10: 'pending', 20: 'settled', 30: 'refunded', 40: 'failed' } as const)[status as 10 | 20 | 30 | 40] ?? 'unknownStatus'
export const netAmount = (row: LogRow) => row.net_amount_micro ?? (row.status === 20 ? row.amount_micro : 0)
export const cacheRead = (row: Pick<LogRow, 'usage'>) => row.usage.cache_read_reported === true || row.usage.cached_tokens > 0 ? row.usage.cached_tokens : null
export const cacheWrite = (row: Pick<LogRow, 'usage'>) => row.usage.cache_write_reported === true || (row.usage.cache_write_tokens ?? 0) > 0 ? row.usage.cache_write_tokens ?? null : null

// Only a known read with a valid input denominator can express cache coverage.
export function cacheReadShare(row: Pick<LogRow, 'usage'>, locale: string): string | null {
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
export function billingLines(row: Pick<LogRow, 'pricing_snapshot' | 'usage' | 'usage_details_recorded'>) {
  const s = row.pricing_snapshot, u = row.usage
  const unit = scaled(s?.final_unit_price_input_per_1m_usd)
  if (s && unit !== null && (u.input_unit ?? s.input_unit) === 'characters') {
    const characters = u.input_characters ?? s.input_characters
    return characters != null && Number.isSafeInteger(characters) && characters >= 0
      ? [{ name: 'inputCharacters', quantity: characters, unitMicro: Number(unit), amountMicro: Number(BigInt(characters) * unit) / 1_000_000 }] : []
  }
  if (!s || unit === null || !row.usage_details_recorded || !['ratio', 'tiered'].includes(s.mode)) return []
  if ([u.cache_write_tokens, u.audio_prompt_tokens, u.image_prompt_tokens, u.audio_completion_tokens].some((n) => n == null || !Number.isSafeInteger(n) || n < 0)) return []
  const normal = u.prompt_tokens - u.cached_tokens - (u.cache_write_tokens ?? 0) - (u.audio_prompt_tokens ?? 0) - (u.image_prompt_tokens ?? 0)
  // Old text-only snapshots predate image output. New snapshots must supply it.
  const imageOut = u.image_completion_tokens ?? s.image_completion_tokens ?? (s.modality_ratios == null ? 0 : null)
  if (imageOut == null || !Number.isSafeInteger(imageOut) || imageOut < 0) return []
  const textOut = u.completion_tokens - (u.audio_completion_tokens ?? 0) - imageOut
  const readModal = u.cache_read_modalities, writeModal = u.cache_write_modalities
  const readText = u.cached_tokens - (readModal?.audio_tokens ?? 0) - (readModal?.image_tokens ?? 0)
  const writeText = (u.cache_write_tokens ?? 0) - (writeModal?.audio_tokens ?? 0) - (writeModal?.image_tokens ?? 0)
  if (normal < 0 || textOut < 0 || readText < 0 || writeText < 0) return []
  const audio = scaled(s.audio_ratio), audioOut = scaled(s.audio_completion_ratio)
  const audioOutputRatio = audio !== null && audioOut !== null ? (audio === 1_000_000n && audioOut === 1_000_000n ? scaled(s.completion_ratio) : audio * audioOut / 1_000_000n) : null
  const hasTtlPrice = s.modality_ratios?.cache_write_5m != null || s.modality_ratios?.cache_write_1h != null
  const short = u.cache_write_5m_tokens, long = u.cache_write_1h_tokens
  if (hasTtlPrice && (short == null || long == null || !Number.isSafeInteger(short) || !Number.isSafeInteger(long)
    || short < 0 || long < 0 || short + long !== u.cache_write_tokens || writeText !== u.cache_write_tokens)) return []
  const writes: [string, number | null, bigint | null][] = hasTtlPrice ? [
    ['cacheWrite5m', short!, scaled(s.modality_ratios?.cache_write_5m ?? s.cache_write_ratio)],
    ['cacheWrite1h', long!, scaled(s.modality_ratios?.cache_write_1h ?? s.cache_write_ratio)],
  ] : [[writeText === (u.cache_write_tokens ?? 0) ? 'cacheWrite' : 'cacheWriteText', cacheWrite(row) === null ? null : writeText, scaled(s.cache_write_ratio)]]
  const parts: [string, number | null, bigint | null][] = [
    ['normalInput', normal, 1_000_000n],
    [readText === u.cached_tokens ? 'cacheRead' : 'cacheReadText', cacheRead(row) === null ? null : readText, scaled(s.cache_ratio)],
    ...writes,
    ['textOutput', textOut, scaled(s.completion_ratio)],
    ...(u.audio_prompt_tokens ? [['audioInput', u.audio_prompt_tokens, audio] as [string, number, bigint | null]] : []),
    ...(u.image_prompt_tokens ? [['imageInput', u.image_prompt_tokens, scaled(s.image_ratio)] as [string, number, bigint | null]] : []),
    ...(u.audio_completion_tokens ? [['audioOutput', u.audio_completion_tokens, audioOutputRatio] as [string, number, bigint | null]] : []),
    ...(imageOut ? [['imageOutput', imageOut, scaled(s.modality_ratios?.image_output)] as [string, number, bigint | null]] : []),
    ...(readModal?.audio_tokens ? [['cacheReadAudio', readModal.audio_tokens, scaled(s.modality_ratios?.audio_cache_read)] as [string, number, bigint | null]] : []),
    ...(readModal?.image_tokens ? [['cacheReadImage', readModal.image_tokens, scaled(s.modality_ratios?.image_cache_read)] as [string, number, bigint | null]] : []),
    ...(writeModal?.audio_tokens ? [['cacheWriteAudio', writeModal.audio_tokens, scaled(s.modality_ratios?.audio_cache_write)] as [string, number, bigint | null]] : []),
    ...(writeModal?.image_tokens ? [['cacheWriteImage', writeModal.image_tokens, scaled(s.modality_ratios?.image_cache_write)] as [string, number, bigint | null]] : []),
  ]
  if (parts.some(([, quantity]) => quantity !== null && (!Number.isSafeInteger(quantity) || quantity < 0))) return []
  return parts.map(([name, quantity, ratio]) => ({ name, quantity,
    unitMicro: ratio === null ? null : Number(unit * ratio) / 1_000_000,
    amountMicro: ratio === null || quantity === null ? null : Number(unit * ratio * BigInt(quantity)) / 1_000_000_000_000,
  }))
}
