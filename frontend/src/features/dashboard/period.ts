import type { TFunction } from 'i18next'
import type { DateRange } from '@/components/ui/date-range'
import type { AnalyticsSearch } from '@/routes/admin.stats'
import { effectiveDays } from '@/features/analytics/search'
import { qk } from '@/lib/query-keys'

export function dashboardSearch(days: number, range?: DateRange | null): AnalyticsSearch {
  return range ? { days: effectiveDays({ start_date: range.start, end_date: range.end }), start_date: range.start, end_date: range.end } : { days }
}

export function dashboardPeriodLabel(days: number, range: DateRange | null | undefined, t: TFunction): string {
  return range ? t('charts:customRange') : days === 1 ? t('admin:kpiToday') : t('admin:lastDays', { days })
}

export function dashboardOverviewKey(days: number, range?: DateRange | null) {
  return range ? [...qk.statsOverview(days), range.start, range.end] as const : qk.statsOverview(days)
}

function date(value: unknown): string | undefined {
  if (typeof value !== 'string' || !/^\d{4}-\d{2}-\d{2}$/.test(value)) return undefined
  const parsed = new Date(`${value}T00:00:00Z`)
  return Number.isFinite(parsed.getTime()) && parsed.toISOString().slice(0, 10) === value && value >= '1970-01-01' && value <= '2148-12-31' ? value : undefined
}

// Ignore the retired KPI-only `scope` parameter. One range now controls the page.
export function dashboardPeriodSearch(search: Record<string, unknown>) {
  const start = date(search.start_date), end = date(search.end_date)
  const validRange = start && end && start <= end && effectiveDays({ start_date: start, end_date: end, days: 367 }) <= 366
  return {
    days: [1, 7, 30].includes(Number(search.days)) ? Number(search.days) : undefined,
    start_date: validRange ? start : undefined,
    end_date: validRange ? end : undefined,
  }
}
