import type { AnalyticsSearch } from '@/routes/admin.stats'

export const DEFAULT_DAYS = 7

/// 可作过滤的五个维度（与后端 CubeQuery 一一对应）。
export const FILTER_DIMS = ['user_id', 'api_key_id', 'channel_id', 'model', 'group'] as const
export type FilterDim = (typeof FILTER_DIMS)[number]

export function effectiveDays(s: AnalyticsSearch): number {
  if (s.start_date && s.end_date) {
    const days = Math.round((Date.parse(`${s.end_date}T00:00:00Z`) - Date.parse(`${s.start_date}T00:00:00Z`)) / 86400_000) + 1
    if (days > 0 && days <= 366) return days
  }
  return s.days ?? DEFAULT_DAYS
}

/// URL search → 立方体端点的 query string（只带过滤维度与时间窗；视图参数另加）。
export function cubeParams(s: AnalyticsSearch, extra?: Record<string, string | undefined>): string {
  const p = new URLSearchParams()
  p.set('days', String(effectiveDays(s)))
  if (s.user_id !== undefined) p.set('user_id', String(s.user_id))
  if (s.api_key_id !== undefined) p.set('api_key_id', String(s.api_key_id))
  if (s.channel_id !== undefined) p.set('channel_id', String(s.channel_id))
  if (s.model !== undefined) p.set('model', s.model)
  if (s.group !== undefined) p.set('group', s.group)
  for (const key of ['start_date', 'end_date', 'granularity', 'model_source', 'endpoint', 'upstream_endpoint', 'node', 'request_type', 'billing_type', 'stream'] as const) {
    if (s[key] !== undefined) p.set(key, String(s[key]))
  }
  for (const key of ['models', 'groups'] as const) { if (s[key]?.length) p.set(key, JSON.stringify(s[key])) }
  for (const [k, v] of Object.entries(extra ?? {})) {
    if (v !== undefined && v !== '') p.set(k, v)
  }
  return p.toString()
}

export function hasFilter(s: AnalyticsSearch): boolean {
  return FILTER_DIMS.some((d) => s[d] !== undefined)
}

/// 去掉 URL 里等于缺省值的键，保持地址干净。
export function cleanSearch(s: AnalyticsSearch): AnalyticsSearch {
  const out: AnalyticsSearch = { ...s }
  if (out.days === DEFAULT_DAYS) delete out.days
  if (out.view === 'trend') delete out.view
  if (out.by === 'model') delete out.by
  if (out.metric === 'amount') delete out.metric
  for (const k of Object.keys(out) as (keyof AnalyticsSearch)[]) {
    if (out[k] === undefined) delete out[k]
  }
  return out
}
