import type { Scope } from './types'
import { calendarRangeSearch } from '@/lib/calendar-range'
import { oneOf, text } from '@/lib/search-params'
import { USAGE_METRICS } from './usage-chart-data'
import type { UsageMetric } from './usage-chart-data'

export const PORTAL_VIEWS = ['overview', 'trend', 'models', 'tokens'] as const
export type PortalView = (typeof PORTAL_VIEWS)[number]
export const MODEL_METRICS = ['amount', 'requests', 'tokens'] as const
export type ModelMetric = (typeof MODEL_METRICS)[number]
export interface PortalOverviewSearch {
  scope?: Scope
  days?: number
  view?: PortalView
  start_date?: string
  end_date?: string
  measure?: UsageMetric
  model_measure?: ModelMetric
  model_query?: string
}

export function overviewSearch(search: Record<string, unknown>): PortalOverviewSearch {
  return {
    scope: search.scope === 'user' || search.scope === 'key' ? search.scope : undefined,
    days: [1, 7, 30, 90].includes(Number(search.days)) ? Number(search.days) : undefined,
    view: search.view === 'trend' || search.view === 'models' || search.view === 'tokens' ? search.view : undefined,
    ...calendarRangeSearch(search),
    measure: oneOf(search.measure, USAGE_METRICS),
    model_measure: oneOf(search.model_measure, MODEL_METRICS),
    // 实时搜索保留输入中的空格，避免输入第二个词时光标被路由清理打断。
    model_query: typeof search.model_query === 'string' ? search.model_query.slice(0, 256) || undefined : text(search.model_query),
  }
}
