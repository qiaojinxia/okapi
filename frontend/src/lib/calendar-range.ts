function calendarDate(value: unknown): string | undefined {
  if (typeof value !== 'string' || !/^\d{4}-\d{2}-\d{2}$/.test(value) || value < '1970-01-01' || value > '2148-12-31') return undefined
  const time = Date.parse(`${value}T00:00:00Z`)
  return Number.isFinite(time) && new Date(time).toISOString().slice(0, 10) === value ? value : undefined
}

export function calendarRangeSearch(search: Record<string, unknown>): { start_date?: string; end_date?: string } {
  const start = calendarDate(search.start_date), end = calendarDate(search.end_date)
  const length = start && end ? (Date.parse(end) - Date.parse(start)) / 86400_000 + 1 : 0
  return length > 0 && length <= 366 ? { start_date: start, end_date: end } : { start_date: undefined, end_date: undefined }
}

export function todayInTimezone(timezone: string): string {
  try {
    const parts = new Intl.DateTimeFormat('en', { timeZone: timezone, year: 'numeric', month: '2-digit', day: '2-digit' }).formatToParts(new Date())
    return ['year', 'month', 'day'].map((type) => parts.find((part) => part.type === type)?.value).join('-')
  } catch {
    return new Date().toISOString().slice(0, 10)
  }
}
