import type { BreakdownRow, Scope } from '@/features/portal-overview/types'

export interface ActivityResponse {
  year: number
  today: string
  timezone: string
  first_year: number
  scope: Scope
  data: BreakdownRow[]
}

export type Metric = 'tokens' | 'requests' | 'amount_micro'
export interface ActivityDay {
  day: string
  tokens: number
  requests: number
  amount_micro: number
  prompt_tokens: number
  completion_tokens: number
  cached_tokens: number
  cache_write_tokens: number | null
  reasoning_tokens: number
  errors: number
  models: BreakdownRow[]
}

export function dateKey(date: Date): string {
  return date.toISOString().slice(0, 10)
}

// 日聚合已按服务端时区落桶。日期作为纯日历值，用 UTC 运算避免夏令时跳日。
export function calendarDate(day: string): Date {
  return new Date(`${day}T00:00:00Z`)
}

export function buildActivity(response: ActivityResponse) {
  const days: ActivityDay[] = []
  const lookup = new Map<string, ActivityDay>()
  const date = new Date(Date.UTC(response.year, 0, 1))
  while (date.getUTCFullYear() === response.year) {
    const day: ActivityDay = { day: dateKey(date), tokens: 0, requests: 0, amount_micro: 0,
      prompt_tokens: 0, completion_tokens: 0, cached_tokens: 0, cache_write_tokens: 0, reasoning_tokens: 0, errors: 0, models: [] }
    days.push(day)
    lookup.set(day.day, day)
    date.setUTCDate(date.getUTCDate() + 1)
  }
  for (const row of response.data) {
    const day = lookup.get(row.day)
    if (!day || row.day > response.today) continue
    day.models.push(row)
    // 缓存属于输入、推理属于输出；不可重复加入 Token 总量。
    day.tokens += row.prompt_tokens + row.completion_tokens
    day.cache_write_tokens = day.cache_write_tokens == null || row.cache_write_tokens == null ? null : day.cache_write_tokens + row.cache_write_tokens
    for (const field of ['requests', 'amount_micro', 'prompt_tokens', 'completion_tokens',
      'cached_tokens', 'reasoning_tokens', 'errors'] as const) day[field] += row[field]
  }
  let streak = 0
  let longestStreak = 0
  const total = { tokens: 0, requests: 0, amount_micro: 0, activeDays: 0 }
  for (const day of days) {
    total.tokens += day.tokens
    total.requests += day.requests
    total.amount_micro += day.amount_micro
    if (day.requests > 0) {
      total.activeDays++
      longestStreak = Math.max(longestStreak, ++streak)
    } else streak = 0
  }
  return { days, lookup, total, longestStreak }
}

// 非零值按当年分位划四档，避免单日极大值把全年压成同一浅色。
export function intensityThresholds(days: ActivityDay[], metric: Metric): number[] {
  const values = days.map((d) => d[metric]).filter((n) => n > 0).sort((a, b) => a - b)
  return [0.25, 0.5, 0.75].map((q) => values[Math.max(0, Math.ceil(values.length * q) - 1)] ?? 0)
}

export function intensity(value: number, thresholds: number[]): number {
  return value <= 0 ? 0 : 1 + thresholds.filter((threshold) => value > threshold).length
}

export const heatColors = [
  'bg-muted border-border/60',
  'bg-emerald-200 border-emerald-300/70 dark:bg-emerald-950 dark:border-emerald-800',
  'bg-emerald-400 border-emerald-500/30 dark:bg-emerald-800 dark:border-emerald-600/50',
  'bg-emerald-600 border-emerald-700/30 dark:bg-emerald-600 dark:border-emerald-400/40',
  'bg-emerald-800 border-emerald-900/30 dark:bg-emerald-400 dark:border-emerald-300/60',
]
