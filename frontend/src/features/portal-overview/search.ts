import type { Scope } from './types'
import { calendarRangeSearch } from '@/lib/calendar-range'
import { oneOf } from '@/lib/search-params'
import { USAGE_METRICS } from './usage-chart-data'
import type { UsageMetric } from './usage-chart-data'

/// 总览曾分页签；现在是一页，旧链接的 `view` 只用来在打开时滚到趋势（trend）或模型排行（models / tokens）。
export const PORTAL_VIEWS = ['overview', 'trend', 'models', 'tokens'] as const
export type PortalView = (typeof PORTAL_VIEWS)[number]
export interface PortalOverviewSearch {
  scope?: Scope
  days?: number
  view?: PortalView
  start_date?: string
  end_date?: string
  measure?: UsageMetric
}

export function overviewSearch(search: Record<string, unknown>): PortalOverviewSearch {
  return {
    scope: search.scope === 'user' || search.scope === 'key' ? search.scope : undefined,
    days: [1, 7, 30, 90].includes(Number(search.days)) ? Number(search.days) : undefined,
    view: search.view === 'trend' || search.view === 'models' || search.view === 'tokens' ? search.view : undefined,
    ...calendarRangeSearch(search),
    measure: oneOf(search.measure, USAGE_METRICS),
  }
}
