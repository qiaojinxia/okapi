import { createFileRoute } from '@tanstack/react-router'
import { TREND_METRICS } from '@/features/analytics/trend-data'
import { AnalyticsPage } from '@/features/analytics/AnalyticsPage'
import { advancedSearch } from '@/features/analytics/advanced-search'

import { ANALYTICS_VIEWS, BREAKDOWN_DIMS, STACK_DIMS, FLOW_METRICS } from '@/features/analytics/route-state'
import type { AnalyticsSearch } from '@/features/analytics/route-state'
export type { AnalyticsSearch, AnalyticsView, BreakdownDim, StackDim, FlowMetric } from '@/features/analytics/route-state'

function str(v: unknown): string | undefined {
  return typeof v === 'string' && v.trim() !== '' ? v.trim() : undefined
}

function posInt(v: unknown): number | undefined {
  const n = typeof v === 'number' ? v : typeof v === 'string' ? Number(v) : Number.NaN
  return Number.isInteger(n) && n > 0 ? n : undefined
}

function oneOf<T extends string>(v: unknown, allowed: readonly T[]): T | undefined {
  return typeof v === 'string' && (allowed as readonly string[]).includes(v) ? (v as T) : undefined
}

function strings(value: unknown): string[] | undefined {
  if (typeof value === 'string') { try { value = JSON.parse(value) } catch { return undefined } }
  return Array.isArray(value) && value.length <= 8 && value.every((v) => typeof v === 'string' && v.length > 0 && v.length <= 256) ? value : undefined
}

export const Route = createFileRoute('/admin/stats')({
  validateSearch: (search: Record<string, unknown>): AnalyticsSearch => ({
    ...advancedSearch(search),
    user_id: posInt(search.user_id),
    api_key_id: posInt(search.api_key_id),
    channel_id: posInt(search.channel_id),
    model: str(search.model),
    group: str(search.group),
    days: posInt(search.days),
    view: oneOf(search.view, ANALYTICS_VIEWS),
    by: oneOf(search.by, BREAKDOWN_DIMS),
    stack: oneOf(search.stack, STACK_DIMS),
    measure: oneOf(search.measure, TREND_METRICS),
    stages: strings(search.stages), limit: posInt(search.limit),
    metric: oneOf(search.metric, FLOW_METRICS),
  }),
  component: AnalyticsPage,
})
