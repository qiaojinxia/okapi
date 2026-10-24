import { trendChart } from '@/features/analytics/trend-data'
import type { TrendBucket, TrendResp } from '@/features/analytics/types'

export type SparkMeasure = 'tokens' | 'requests' | 'amount' | 'error_rate'

/// 指标卡里的迷你趋势：逐桶取值，空日补零，与下方趋势图同一份精简查询。
/// 天粒度去掉还在累计的今天（半天的量会让曲线末尾骤降）；小时粒度去掉尚未发生的尾部空桶。
/// 没有数据（或查询失败）时返回空数组，卡片就不画线。
export function sparkValues(resp: TrendResp | undefined, measure: SparkMeasure): number[] {
  if (!resp || resp.stack || resp.data.length === 0) return []
  const rows = new Map((resp.data as TrendBucket[]).map((row) => [row.bucket, row]))
  const buckets = trendChart(resp, 'requests', '', '', '', '').data.map((point) => point.bucket)
  const value = (bucket: string): number => {
    const row = rows.get(bucket)
    if (!row) return 0
    if (measure === 'tokens') return row.tokens ?? 0
    if (measure === 'requests') return row.requests ?? 0
    if (measure === 'amount') return (row.amount_micro ?? 0) / 1_000_000
    return (row.requests ?? 0) > 0 ? (row.errors ?? 0) / row.requests * 100 : 0
  }
  if (resp.granularity === 'day') {
    const today = resp.window?.today
    return buckets.filter((bucket) => bucket !== today).map(value)
  }
  const last = buckets.reduce((found, bucket, i) => (rows.get(bucket)?.requests ?? 0) > 0 ? i : found, -1)
  return buckets.slice(0, last + 1).map(value)
}
