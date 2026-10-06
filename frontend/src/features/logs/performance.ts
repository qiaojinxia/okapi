import type { LogRow } from './types'

type PerformanceRow = Pick<LogRow, 'is_stream' | 'ttft_ms' | 'latency_ms' | 'usage'>

/** Paired, known Token measurements only. Never infer a unit from a model name. */
export function outputRates(row: PerformanceRow): { average: number | null; generation: number | null } {
  const tokens = row.usage.completion_tokens, total = row.latency_ms, ttft = row.ttft_ms
  const valid = row.usage.input_unit === 'tokens' && row.usage.input_characters == null
    && Number.isSafeInteger(tokens) && tokens >= 0
    && total != null && Number.isFinite(total) && total > 0
  const average = valid ? tokens * 1000 / total : null
  const generation = valid && row.is_stream && ttft != null && Number.isFinite(ttft)
    && ttft >= 0 && total > ttft && tokens > 0 ? tokens * 1000 / (total - ttft) : null
  return {
    average: average != null && Number.isFinite(average) ? average : null,
    generation: generation != null && Number.isFinite(generation) ? generation : null,
  }
}

export function formatOutputRate(value: number | null, locale: string, compact = false): string {
  if (value == null || !Number.isFinite(value)) return '—'
  return `${value.toLocaleString(locale, { maximumFractionDigits: 1, notation: compact && value >= 10_000 ? 'compact' : 'standard' })} tok/s`
}
