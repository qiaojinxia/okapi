import type { CacheMetrics } from './types'

function validRate(value: number | null | undefined): value is number {
  return value != null && Number.isFinite(value) && value >= 0 && value <= 10_000
}

/** Prefer the complete rate; partial rates describe only paired upstream samples. */
export function cacheHit(value: CacheMetrics & { requests: number }):
  { bp: number; partial: true; samples: number } | { bp: number | null; partial: false; samples: number } {
  if (validRate(value.cache_hit_bp)) {
    return { bp: value.cache_hit_bp, partial: false, samples: value.requests }
  }
  const samples = value.measured_cache_hit_requests ?? 0
  if (samples > 0 && validRate(value.measured_cache_hit_bp)) {
    return { bp: value.measured_cache_hit_bp, partial: true, samples }
  }
  return { bp: null, partial: false, samples: 0 }
}

/** Aggregate paired observations without diluting them with unknown prompts. */
export function aggregateCacheHit(rows: (CacheMetrics & { requests: number; prompt_tokens: number; cached_tokens: number })[]) {
  let prompt = 0, cached = 0, samples = 0, requests = 0
  for (const row of rows) {
    requests += row.requests
    const observation = cacheHit(row)
    if (observation.bp == null) continue
    const measured = row.measured_prompt_tokens
    if (measured != null && measured > 0 && row.measured_cache_read_tokens != null) {
      prompt += measured
      cached += row.measured_cache_read_tokens
      samples += row.measured_cache_hit_requests ?? observation.samples
    } else if (!observation.partial && row.prompt_tokens > 0) {
      prompt += row.prompt_tokens
      cached += row.cached_tokens
      samples += observation.samples
    }
  }
  return { bp: prompt > 0 ? Math.round(cached * 10_000 / prompt) : null, samples, partial: samples < requests }
}

/** A missing full rate does not erase known cache-read quantities. */
export function cacheReadKnown(value: { cached_tokens: number; cache_read_known_requests?: number; cache_hit_bp?: number | null }) {
  return value.cached_tokens > 0 || (value.cache_read_known_requests ?? 0) > 0 || validRate(value.cache_hit_bp)
}

/** Recorded quantities survive incomplete coverage; zero requires an observation. */
export function cacheAmount(value: {
  requests: number
  cached_tokens: number
  cache_read_known_requests?: number
  cache_hit_bp?: number | null
  cache_write_tokens?: number | null
  recorded_cache_write_tokens?: number | null
  cache_write_known_requests?: number
}, kind: 'read' | 'write'): { tokens: number | null; partial: boolean } {
  const samples = kind === 'read' ? value.cache_read_known_requests : value.cache_write_known_requests
  const tokens = kind === 'read'
    ? cacheReadKnown(value) ? value.cached_tokens : null
    : value.recorded_cache_write_tokens ?? value.cache_write_tokens ?? null
  if (tokens == null || (tokens === 0 && value.requests === 0)) return { tokens: null, partial: false }
  const complete = value.requests > 0 && (samples == null
    ? kind === 'read' ? validRate(value.cache_hit_bp) : value.cache_write_tokens != null
    : samples === value.requests)
  return { tokens, partial: !complete }
}
