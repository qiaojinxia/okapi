import type { BreakdownRow } from './types'
import { chartColor, OTHER_COLOR } from '../../lib/chart'
import type { ChartPoint, ChartSeries } from '@/components/ui/time-chart'

export const USAGE_METRICS = ['amount', 'requests', 'tokens', 'original', 'cache', 'success', 'latency'] as const
export type UsageMetric = typeof USAGE_METRICS[number]

export function calendarDays(start: string, end: string): string[] {
  const date = new Date(`${start}T00:00:00Z`)
  const last = new Date(`${end}T00:00:00Z`)
  const days: string[] = []
  while (date <= last && days.length < 367) { days.push(date.toISOString().slice(0, 10)); date.setUTCDate(date.getUTCDate() + 1) }
  return days
}

export function usageValue(row: BreakdownRow, metric: UsageMetric): number {
  return metric === 'amount' ? row.amount_micro / 1_000_000 : metric === 'original' ? (row.original_micro ?? row.amount_micro + row.discount_micro) / 1_000_000
    : metric === 'requests' ? row.requests : row.prompt_tokens + row.completion_tokens
}

export function usageChart(rows: BreakdownRow[], days: string[], metric: UsageMetric, other: string, labels: { total: string; latency: string; ttft: string }) {
  const byDay = new Map<string, BreakdownRow[]>()
  for (const row of rows) byDay.set(row.day, [...(byDay.get(row.day) ?? []), row])
  if (metric === 'cache' || metric === 'success' || metric === 'latency') {
    const data: ChartPoint[] = days.map((bucket) => {
      const group = byDay.get(bucket) ?? []
      const sum = (field: keyof BreakdownRow) => group.reduce((acc, row) => acc + Number(row[field] ?? 0), 0)
      const requests = sum('requests')
      const prompt = sum('prompt_tokens')
      const completePerformance = requests > 0 && group.every((r) => r.performance_requests === r.requests)
      return { bucket, value: metric === 'cache' ? (prompt > 0 ? sum('cached_tokens') / prompt * 100 : null)
        : metric === 'success' ? (requests > 0 ? (requests - sum('errors')) / requests * 100 : null)
          : completePerformance ? sum('latency_sum_ms') / requests : null,
        ttft: completePerformance && sum('ttft_samples') > 0 ? sum('ttft_sum_ms') / sum('ttft_samples') : null }
    })
    const series: ChartSeries[] = [{ key: 'value', label: metric === 'latency' ? labels.latency : labels.total, color: chartColor(0) }]
    if (metric === 'latency') series.push({ key: 'ttft', label: labels.ttft, color: chartColor(1) })
    return { data, series, stacked: false, line: true }
  }
  const rank = new Map<string, number>()
  for (const row of rows) rank.set(row.model, (rank.get(row.model) ?? 0) + usageValue(row, metric))
  const models = [...rank].sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0])).map(([name]) => name)
  const top = models.slice(0, 6)
  const hasOther = models.length > top.length
  // 独立序列 ID 防止带点号、方括号或 __proto__ 的模型名被当成属性路径。
  const series = top.map((label, i) => ({ key: `s${i}`, label, color: chartColor(i) }))
  if (hasOther) series.push({ key: 'other', label: other, color: OTHER_COLOR })
  const data: ChartPoint[] = days.map((bucket) => {
    const point: ChartPoint = { bucket }
    for (const s of series) point[s.key] = 0
    for (const row of byDay.get(bucket) ?? []) {
      const index = top.indexOf(row.model)
      const key = index < 0 ? 'other' : `s${index}`
      point[key] = Number(point[key] ?? 0) + usageValue(row, metric)
    }
    return point
  })
  return { data, series, stacked: true, line: false }
}
