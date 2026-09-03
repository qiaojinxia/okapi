import { useNavigate } from '@tanstack/react-router'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import {
  Area,
  AreaChart,
  Bar,
  BarChart,
  CartesianGrid,
  Legend,
  Line,
  LineChart,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from 'recharts'
import type { AnalyticsSearch, StackDim } from '@/routes/admin.stats'
import { STACK_DIMS } from '@/routes/admin.stats'
import { Card, CardContent } from '@/components/ui/card'
import { Segmented } from '@/components/ui/segmented'
import { EmptyState, ErrorState, LoadingState } from '@/components/ui/state'
import { cleanSearch } from '@/features/analytics/search'
import type { StackedBucket, TrendBucket, TrendResp } from '@/features/analytics/types'
import { OTHER_COLOR, OTHER_KEY, chartColor } from '@/lib/chart'
import { describeError } from '@/lib/i18n'

const METRICS = ['amount', 'requests', 'tokens', 'error_rate', 'latency'] as const
type Metric = (typeof METRICS)[number]
const STACKABLE: readonly Metric[] = ['amount', 'requests', 'tokens']

/// 时间桶标签：小时桶取 "HH:00"，日桶取 "MM-DD"。
function bucketLabel(bucket: string, granularity: 'hour' | 'day'): string {
  return granularity === 'hour' ? bucket.slice(11, 16) : bucket.slice(5, 10)
}

/// 补齐窗口内没有流量的桶（值为 0）。
///
/// 后端只返回有数据的桶：7 天里只有一天有流量时图上只剩一个孤点，读起来像坏了；
/// 补零后曲线落到轴上，"其余六天确实没请求"才被表达出来。桶键与后端同格式
/// （日 `YYYY-MM-DD`、小时 `YYYY-MM-DD HH:00:00`，UTC）。
function fillBuckets<T extends { bucket: string }>(
  rows: T[],
  days: number,
  granularity: 'hour' | 'day',
  empty: (bucket: string) => T,
): T[] {
  const byKey = new Map(rows.map((r) => [r.bucket, r]))
  const out: T[] = []
  const now = new Date()
  const pad = (n: number) => String(n).padStart(2, '0')
  if (granularity === 'day') {
    for (let i = days; i >= 0; i--) {
      const d = new Date(Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), now.getUTCDate() - i))
      const key = `${d.getUTCFullYear()}-${pad(d.getUTCMonth() + 1)}-${pad(d.getUTCDate())}`
      out.push(byKey.get(key) ?? empty(key))
    }
  } else {
    const hours = days * 24
    for (let i = hours; i >= 0; i--) {
      const d = new Date(now.getTime() - i * 3_600_000)
      const key = `${d.getUTCFullYear()}-${pad(d.getUTCMonth() + 1)}-${pad(d.getUTCDate())} ${pad(d.getUTCHours())}:00:00`
      out.push(byKey.get(key) ?? empty(key))
    }
  }
  // 后端桶若因时区/边界落在生成序列之外，保留而不丢
  for (const r of rows) {
    if (!out.some((o) => o.bucket === r.bucket)) out.push(r)
  }
  out.sort((a, b) => a.bucket.localeCompare(b.bucket))
  return out
}

const EMPTY_METRICS = {
  requests: 0,
  errors: 0,
  error_rate_bp: 0,
  prompt_tokens: 0,
  cached_tokens: 0,
  completion_tokens: 0,
  reasoning_tokens: 0,
  tokens: 0,
  cache_hit_bp: 0,
  amount_micro: 0,
  discount_micro: 0,
  upstream_cost_micro: 0,
  avg_latency_ms: 0,
  avg_ttft_ms: 0,
}

/// 趋势视图：一次只看一个度量（消费 / 请求 / Tokens / 错误率 / 时延），
/// 前三者可按第二维度堆叠（"钱花在哪个模型、占比怎么变"）。
///
/// 不做双轴：请求数与金额差几个数量级，双轴图里总有一条被压成直线；
/// 切换度量比同时画两条更快读出走势。堆叠维度进 URL（可深链、可回退），
/// 度量选择是本地状态——它只影响同一份数据的呈现，不值得占地址栏。
export function TrendView({
  search,
  resp,
  isLoading,
  error,
}: {
  search: AnalyticsSearch
  resp: TrendResp | undefined
  isLoading: boolean
  error: unknown
}) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const navigate = useNavigate({ from: '/admin/stats' })
  const [metric, setMetric] = useState<Metric>('amount')
  const stack = search.stack

  const setStack = (v: StackDim | 'none') => {
    void navigate({
      search: (prev) => cleanSearch({ ...prev, stack: v === 'none' ? undefined : v }),
    })
  }
  const pickMetric = (m: Metric) => {
    setMetric(m)
    if (!STACKABLE.includes(m) && stack !== undefined) setStack('none')
  }

  const metricLabel: Record<Metric, string> = {
    amount: t('analytics:kpiSpend'),
    requests: t('common:requests'),
    tokens: t('common:tokens'),
    error_rate: t('admin:errorRate'),
    latency: t('analytics:kpiLatency'),
  }
  const stackLabel: Record<StackDim | 'none', string> = {
    none: t('analytics:stackNone'),
    model: t('analytics:dimModel'),
    channel: t('analytics:dimChannel'),
    group: t('analytics:dimGroup'),
    user: t('analytics:dimUser'),
    api_key: t('analytics:dimApiKey'),
  }

  const fmtValue = (v: number): string => {
    switch (metric) {
      case 'amount':
        return `$${v.toFixed(v >= 100 ? 2 : 4)}`
      case 'error_rate':
        return `${v.toFixed(2)}%`
      case 'latency':
        return `${Math.round(v).toLocaleString(locale)} ms`
      default:
        return Math.round(v).toLocaleString(locale)
    }
  }

  return (
    <Card>
      <CardContent className="flex flex-col gap-3 pt-4">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <Segmented
            options={METRICS.map((m) => ({ value: m, label: metricLabel[m] }))}
            value={metric}
            onChange={pickMetric}
            size="sm"
          />
          <div className="flex items-center gap-2">
            <span className="text-xs text-muted-foreground">{t('analytics:stackBy')}</span>
            <Segmented
              options={(['none', ...STACK_DIMS] as const).map((s) => ({
                value: s,
                label: stackLabel[s],
                disabled: s !== 'none' && !STACKABLE.includes(metric),
              }))}
              value={stack ?? 'none'}
              onChange={setStack}
              size="sm"
            />
          </div>
        </div>

        {error !== null && error !== undefined ? (
          <ErrorState message={describeError(error)} />
        ) : isLoading || resp === undefined ? (
          <LoadingState />
        ) : resp.data.length === 0 ? (
          <EmptyState hint={t('admin:trendEmptyHint')} />
        ) : resp.stack !== undefined && STACKABLE.includes(metric) ? (
          <StackedChart resp={resp} metric={metric} fmt={fmtValue} />
        ) : (
          <SingleChart resp={resp} metric={metric} fmt={fmtValue} />
        )}
      </CardContent>
    </Card>
  )
}

function SingleChart({
  resp,
  metric,
  fmt,
}: {
  resp: TrendResp
  metric: Metric
  fmt: (v: number) => string
}) {
  const { t } = useTranslation()
  const filled = fillBuckets(resp.data as TrendBucket[], resp.days, resp.granularity, (bucket) => ({
    bucket,
    ...EMPTY_METRICS,
  }))
  const rows = filled.map((b) => ({
    bucket: bucketLabel(b.bucket, resp.granularity),
    amount: b.amount_micro / 1_000_000,
    requests: b.requests,
    tokens: b.tokens,
    error_rate: b.error_rate_bp / 100,
    latency: b.avg_latency_ms,
    ttft: b.avg_ttft_ms,
  }))

  if (metric === 'latency') {
    return (
      <div className="h-80">
        <ResponsiveContainer width="100%" height="100%">
          <LineChart data={rows}>
            <CartesianGrid strokeDasharray="3 3" className="stroke-border" />
            <XAxis dataKey="bucket" fontSize={11} />
            <YAxis fontSize={11} />
            <Tooltip formatter={(v) => fmt(Number(v))} />
            <Legend itemSorter={null} />
            <Line
              type="monotone"
              dataKey="latency"
              name={t('analytics:kpiLatency')}
              stroke="var(--color-chart-1)"
              dot={false}
              isAnimationActive={false}
            />
            <Line
              type="monotone"
              dataKey="ttft"
              name={t('analytics:ttft')}
              stroke="var(--color-chart-2)"
              dot={false}
              isAnimationActive={false}
            />
          </LineChart>
        </ResponsiveContainer>
      </div>
    )
  }

  const color = metric === 'error_rate' ? 'var(--color-destructive)' : 'var(--color-primary)'
  return (
    <div className="h-80">
      <ResponsiveContainer width="100%" height="100%">
        <AreaChart data={rows}>
          <defs>
            <linearGradient id="g-trend" x1="0" y1="0" x2="0" y2="1">
              <stop offset="0%" stopColor={color} stopOpacity={0.35} />
              <stop offset="100%" stopColor={color} stopOpacity={0.02} />
            </linearGradient>
          </defs>
          <CartesianGrid strokeDasharray="3 3" className="stroke-border" />
          <XAxis dataKey="bucket" fontSize={11} />
          <YAxis fontSize={11} tickFormatter={(v) => (metric === 'error_rate' ? `${v}%` : String(v))} />
          <Tooltip formatter={(v) => fmt(Number(v))} />
          <Area
            type="monotone"
            dataKey={metric}
            name={t(`analytics:metric_${metric}`)}
            stroke={color}
            fill="url(#g-trend)"
            isAnimationActive={false}
          />
        </AreaChart>
      </ResponsiveContainer>
    </div>
  )
}

function StackedChart({
  resp,
  metric,
  fmt,
}: {
  resp: TrendResp
  metric: Metric
  fmt: (v: number) => string
}) {
  const { t } = useTranslation()
  const series = resp.series ?? []
  const field = metric === 'amount' ? 'amount_micro' : metric === 'tokens' ? 'tokens' : 'requests'
  const filled = fillBuckets<StackedBucket>(
    resp.data as StackedBucket[],
    resp.days,
    resp.granularity,
    (bucket) => ({ bucket, values: {} }),
  )
  const rows = filled.map((b) => {
    const row: Record<string, number | string> = { bucket: bucketLabel(b.bucket, resp.granularity) }
    for (const s of series) {
      const raw = b.values[s.key]?.[field] ?? 0
      row[s.key] = metric === 'amount' ? raw / 1_000_000 : raw
    }
    return row
  })
  // 模型 / 分组的键本身就是名字；用户 / 密钥 / 渠道的键是 id，回填不到名字时才带 #
  const idKeyed = resp.stack === 'user' || resp.stack === 'api_key' || resp.stack === 'channel'
  const label = (s: { key: string; label: string | null }) =>
    s.key === OTHER_KEY ? t('analytics:other') : (s.label ?? (idKeyed ? `#${s.key}` : s.key))

  return (
    <div className="h-80">
      <ResponsiveContainer width="100%" height="100%">
        <BarChart data={rows}>
          <CartesianGrid strokeDasharray="3 3" className="stroke-border" />
          <XAxis dataKey="bucket" fontSize={11} />
          <YAxis fontSize={11} />
          <Tooltip formatter={(v, name) => [fmt(Number(v)), String(name)]} />
          <Legend itemSorter={null} />
          {series.map((s, idx) => (
            <Bar
              key={s.key}
              dataKey={s.key}
              name={label(s)}
              stackId="s"
              fill={s.key === OTHER_KEY ? OTHER_COLOR : chartColor(idx)}
              isAnimationActive={false}
            />
          ))}
        </BarChart>
      </ResponsiveContainer>
    </div>
  )
}
