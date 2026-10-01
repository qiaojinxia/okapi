import { advancedSearch } from '@/features/analytics/advanced-search'
import type { AdvancedSearch } from '@/features/analytics/advanced-search'
import { oneOf } from '@/lib/search-params'
import { pageSearch } from '@/hooks/use-pagination'
import type { PageSearch } from '@/hooks/use-pagination'

export const QUALITY_TABS = ['trend', 'channels', 'models', 'errors', 'clients'] as const
export type QualityTab = (typeof QUALITY_TABS)[number]
export const QUALITY_METRICS = ['error_rate', 'latency', 'ttft', 'throughput'] as const
export const QUALITY_COMPARISONS = ['model', 'model_group', 'group', 'channel', 'node'] as const

export interface QualitySearch extends AdvancedSearch, PageSearch {
  days?: number
  tab?: QualityTab
  measure?: (typeof QUALITY_METRICS)[number]
  stack?: (typeof QUALITY_COMPARISONS)[number]
}

export function qualitySearch(search: Record<string, unknown>): QualitySearch {
  return {
    ...advancedSearch(search),
    ...pageSearch(search),
    days: [1, 7, 30].includes(Number(search.days)) ? Number(search.days) : undefined,
    tab: oneOf(search.tab, QUALITY_TABS),
    measure: oneOf(search.measure, QUALITY_METRICS),
    stack: oneOf(search.stack, QUALITY_COMPARISONS),
  }
}
