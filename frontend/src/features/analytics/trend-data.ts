import type { ChartPoint, ChartSeries } from '@/components/ui/time-chart'
import type { CubeMetrics, StackedBucket, TrendBucket, TrendResp } from './types'
import { chartColor, OTHER_COLOR, OTHER_KEY } from '../../lib/chart'

export const TREND_METRICS = ['amount', 'requests', 'tokens', 'error_rate', 'cache', 'latency', 'ttft', 'throughput'] as const
export type TrendMetric = typeof TREND_METRICS[number]
export const STACKABLE: readonly TrendMetric[] = ['amount', 'requests', 'tokens']

function metricValue(r: CubeMetrics | undefined, metric: TrendMetric): number | null {
  if (metric === 'amount') return (r?.amount_micro ?? 0) / 1_000_000
  if (metric === 'requests' || metric === 'tokens') return r?.[metric] ?? 0
  if (!r || r.requests <= 0) return null
  if (metric === 'error_rate') return r.error_rate_bp / 100
  if (metric === 'cache') return r.prompt_tokens > 0 ? r.cache_hit_bp / 100 : null
  if (metric === 'ttft') return (r.ttft_samples ?? r.avg_ttft_ms) > 0 ? r.avg_ttft_ms : null
  if (metric === 'throughput') return r.avg_output_tps_milli == null ? null : r.avg_output_tps_milli / 1000
  return r.avg_latency_ms
}
function seriesLabel(key: string, label: string | null, unknown: string): string {
  if (!key) return unknown
  if (label) return label
  // Composite keys remain lossless JSON in the API; the legend is human readable.
  if (key.startsWith('[')) { try { const parts: unknown = JSON.parse(key); if (Array.isArray(parts) && parts.every((p) => typeof p === 'string')) return parts.map((p) => p || unknown).join(' · ') } catch { /* ordinary model name */ } }
  return key
}
function keys(resp: TrendResp): string[] {
  const now = new Date()
  const from = resp.window?.start_at ?? new Date(now.getTime() - resp.days * 86400_000).toISOString().slice(0, 19).replace('T', ' ')
  const to = resp.window?.end_at ?? now.toISOString().slice(0, 19).replace('T', ' ')
  // 字符串是服务端的本地日历桶；UTC 仅作为不受客户端夏令时影响的日历运算。
  const date = new Date(`${from.replace(' ', 'T')}Z`)
  const end = new Date(`${to.replace(' ', 'T')}Z`)
  if (resp.granularity === 'day') { date.setUTCHours(0, 0, 0, 0); end.setUTCHours(0, 0, 0, 0) }
  else { date.setUTCMinutes(0, 0, 0); end.setUTCMinutes(0, 0, 0) }
  const result = new Set<string>()
  while (date <= end && result.size < 2200) {
    result.add(resp.granularity === 'day' ? date.toISOString().slice(0, 10) : date.toISOString().slice(0, 19).replace('T', ' '))
    date.setTime(date.getTime() + (resp.granularity === 'day' ? 86400_000 : 3600_000))
  }
  // 兼容旧服务端的边界桶，不静默丢掉已返回的数据。
  for (const row of resp.data) result.add(row.bucket)
  return [...result].sort()
}

export function trendChart(resp: TrendResp, metric: TrendMetric, label: string, ttft: string, other: string, unknown: string) {
  const buckets = keys(resp)
  if (resp.stack) {
    const source = resp.series ?? []
    const series: ChartSeries[] = source.map((s, i) => ({ key: `s${i}`, label: s.key === OTHER_KEY ? other : seriesLabel(s.key, s.label, unknown), color: s.key === OTHER_KEY ? OTHER_COLOR : chartColor(i) }))
    const index = new Map((resp.data as StackedBucket[]).map((row) => [row.bucket, row]))
    const data: ChartPoint[] = buckets.map((bucket) => {
      const point: ChartPoint = { bucket }
      for (const [i, s] of source.entries()) point[`s${i}`] = metricValue(index.get(bucket)?.values[s.key], metric)
      return point
    })
    return { series, data, stacked: STACKABLE.includes(metric), line: !STACKABLE.includes(metric) }
  }
  const index = new Map((resp.data as TrendBucket[]).map((row) => [row.bucket, row]))
  const data: ChartPoint[] = buckets.map((bucket) => {
    const r = index.get(bucket)
    const hasRequests = !!r && r.requests > 0
    const value = metricValue(r, metric)
    return { bucket, value, ttft: hasRequests && (r.ttft_samples ?? r.avg_ttft_ms) > 0 ? r.avg_ttft_ms : null }
  })
  const series: ChartSeries[] = [{ key: 'value', label, color: metric === 'error_rate' ? 'var(--color-destructive)' : chartColor(0) }]
  if (metric === 'latency') series.push({ key: 'ttft', label: ttft, color: chartColor(1) })
  return { series, data, stacked: false, line: !STACKABLE.includes(metric) }
}
