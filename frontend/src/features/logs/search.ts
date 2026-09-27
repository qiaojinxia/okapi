import { flag, text, posInt } from '@/lib/search-params'
import { calendarRangeSearch } from '@/lib/calendar-range'

export interface PortalLogSearch {
  scope?: 'key' | 'user'
  model?: string
  errors_only?: true
  api_key_id?: number
  request_id?: string
  start_date?: string
  end_date?: string
  timezone?: string
}

export function portalLogSearch(search: Record<string, unknown>): PortalLogSearch {
  const range = calendarRangeSearch(search)
  const timezone = text(search.timezone)
  const requestId = text(search.request_id, 128)
  return {
    scope: search.scope === 'user' || search.scope === 'key' ? search.scope : undefined,
    model: text(search.model),
    errors_only: flag(search.errors_only),
    api_key_id: posInt(search.api_key_id),
    request_id: requestId && /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(requestId) ? requestId : undefined,
    ...range,
    timezone: range.start_date && timezone && timezone.length <= 128 ? timezone : undefined,
  }
}
