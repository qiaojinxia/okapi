import { DEFAULT_DAYS } from '@/lib/cube'
import type { AnalyticsSearch } from '@/routes/admin.stats'

/// 可作过滤的五个维度（与后端 CubeQuery 一一对应）。
export const FILTER_DIMS = ['user_id', 'api_key_id', 'channel_id', 'model', 'group'] as const
export type FilterDim = (typeof FILTER_DIMS)[number]

/// 日志页「回看小时数」的后端上限（console/logs.rs 把 hours 钳到 1–2160，e2e/analytics-search.spec.ts 钉着两边同值）。
/// 分析窗口更长时再塞 hours 会被静默截成 90 天，对账就漏掉更早的记录。
export const LOG_MAX_HOURS = 2160

/// 分析窗口（天）→ 日志页的时间条件：不超过上限用相对的 hours；更长的改用绝对起点 from
/// （RFC3339 UTC；日志页 from 单给 = 从该时刻到现在）。
export function logsWindow(days: number, now: number = Date.now()): { hours?: number; from?: string } {
  const hours = days * 24
  return hours <= LOG_MAX_HOURS ? { hours } : { from: new Date(now - days * 86_400_000).toISOString() }
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
