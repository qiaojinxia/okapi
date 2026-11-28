import { useNavigate } from '@tanstack/react-router'
import { useTranslation } from 'react-i18next'
import type { AnalyticsSearch, StackDim } from '@/features/analytics/route-state'
import { STACK_DIMS } from '@/features/analytics/route-state'
import { Card } from '@/components/ui/card'
import { Segmented } from '@/components/ui/segmented'
import { Select } from '@/components/ui/select'
import { EmptyState, ErrorState, LoadingState } from '@/components/ui/state'
import { TimeChart } from '@/components/ui/time-chart'
import { WorkspaceHeader } from './WorkspaceHeader'
import { cleanSearch } from './search'
import type { TrendResp } from '@/lib/cube'
import { STACKABLE, TREND_METRICS, trendChart } from './trend-data'
import type { TrendMetric } from './trend-data'
import { dimensionLabel } from './AnalysisControls'
import { describeError } from '@/lib/i18n'
import { formatMoneyAggregate } from '@/lib/money'

function shortBucket(bucket: string): string {
  return bucket.length > 10 ? `${bucket.slice(5, 10)} ${bucket.slice(11, 16)}` : bucket.slice(5)
}

function formatTrendValue(n: number, metric: TrendMetric, locale: string): string {
  if (metric === 'amount') return formatMoneyAggregate(n * 1_000_000, locale, true)
  const ratio = metric === 'cache' || metric === 'error_rate'
  return `${n.toLocaleString(locale, { maximumFractionDigits: ratio || metric === 'throughput' ? 2 : 0 })}${ratio ? '%' : (metric === 'latency' || metric === 'ttft') ? ' ms' : metric === 'throughput' ? ' Token/s' : ''}`
}

export function TrendView({
  tabs,
  search,
  resp,
  isLoading,
  error,
  onRetry,
}: {
  tabs?: React.ReactNode
  search: AnalyticsSearch
  resp: TrendResp | undefined
  isLoading: boolean
  error: unknown
  onRetry: () => void
}) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const navigate = useNavigate({ from: '/admin/stats' })
  const metric: TrendMetric = search.measure ?? 'amount'
  const setStack = (v: StackDim | 'none') => void navigate({ search: (prev) => cleanSearch({ ...prev, stack: v === 'none' ? undefined : v }) })
  const pickMetric = (measure: TrendMetric) => void navigate({ search: (prev) => cleanSearch({ ...prev, measure }) })

  // 从当前响应直接算出峰值与均量（不额外发请求），放在工作区顶栏右侧作为即时洞察。
  let summary: React.ReactNode = null
  if (error == null && !isLoading && resp && resp.data.length > 0) {
    const chart = trendChart(resp, metric, t(`charts:metric_${metric}`), t('analytics:ttft'), t('analytics:other'), t('analysis:notCollected'))
    const additive = STACKABLE.includes(metric)
    if (!resp.stack) {
      const vals = chart.data
        .map((p) => ({ bucket: p.bucket, v: typeof p.value === 'number' ? p.value : null }))
        .filter((x): x is { bucket: string; v: number } => x.v !== null)
      if (vals.length > 0) {
        const peak = vals.reduce((best, cur) => (cur.v > best.v ? cur : best), vals[0])
        const avg = additive ? vals.reduce((sum, cur) => sum + cur.v, 0) / vals.length : null
        summary = (
          <>
            {avg !== null && (
              <span>
                {t('analytics:trendAvg')}{' '}
                <b className="font-semibold text-foreground">{formatTrendValue(avg, metric, locale)}</b>
              </span>
            )}
            {peak.v > 0 && (
              <>
                {avg !== null && <span aria-hidden className="text-border">·</span>}
                <span>
                  {t('analytics:trendPeak')}{' '}
                  <b className="font-semibold text-foreground">{formatTrendValue(peak.v, metric, locale)}</b>
                  <span className="ml-1 text-muted-foreground/80">({shortBucket(peak.bucket)})</span>
                </span>
              </>
            )}
          </>
        )
      }
    } else if (additive && chart.series.length > 0) {
      const totals = chart.data.map((p) => ({
        bucket: p.bucket,
        v: chart.series.reduce((sum, s) => sum + (typeof p[s.key] === 'number' ? (p[s.key] as number) : 0), 0),
      }))
      const peak = totals.reduce((best, cur) => (cur.v > best.v ? cur : best), totals[0])
      if (peak && peak.v > 0) {
        summary = (
          <span>
            {t('analytics:trendPeak')}{' '}
            <b className="font-semibold text-foreground">{formatTrendValue(peak.v, metric, locale)}</b>
            <span className="ml-1 text-muted-foreground/80">({shortBucket(peak.bucket)})</span>
          </span>
        )
      }
    }
  }

  return (
    <Card className="min-w-0 overflow-hidden">
      <WorkspaceHeader tabs={tabs} summary={summary} />
      <div className="flex flex-wrap items-center justify-between gap-3 border-b border-border/55 px-4 py-2.5 sm:px-5">
        <Segmented ariaLabel={t('charts:metric')} options={TREND_METRICS.map((value) => ({ value, label: t(`charts:metric_${value}`) }))} value={metric} onChange={pickMetric} size="sm" />
        <label className="inline-flex min-h-8 items-center gap-2 text-xs text-muted-foreground">
          <span className="whitespace-nowrap">{t(STACKABLE.includes(metric) ? 'analytics:stackBy' : 'analysis:compare')}</span>
          <Select
            size="sm"
            aria-label={t(STACKABLE.includes(metric) ? 'analytics:stackBy' : 'analysis:compare')}
            value={search.stack ?? 'none'}
            onChange={(value) => setStack(value as StackDim | 'none')}
            options={(['none', ...STACK_DIMS] as const).map((value) => ({
              value,
              label: value === 'none' ? t('analytics:stackNone') : dimensionLabel(t, value),
            }))}
          />
        </label>
      </div>
      <div className="px-4 py-4 sm:px-5">
        {error != null ? (
          <ErrorState message={describeError(error)} onRetry={onRetry} />
        ) : isLoading || !resp ? (
          <LoadingState />
        ) : !resp.data.length ? (
          <EmptyState hint={t('admin:trendEmptyHint')} />
        ) : (
          <TrendPlot key={`${metric}-${resp.stack ?? 'none'}`} resp={resp} metric={metric} hideWindowFooter />
        )}
      </div>
    </Card>
  )
}

export function TrendPlot({
  resp,
  metric,
  compact = false,
  paginateTable = false,
  hideWindowFooter = false,
}: {
  resp: TrendResp
  metric: TrendMetric
  compact?: boolean
  paginateTable?: boolean
  hideWindowFooter?: boolean
}) {
  const { t, i18n } = useTranslation()
  const chart = trendChart(resp, metric, t(`charts:metric_${metric}`), t('analytics:ttft'), t('analytics:other'), t('analysis:notCollected'))
  const ratio = metric === 'cache' || metric === 'error_rate'
  const unit = metric === 'amount' ? 'USD' : ratio ? '%' : (metric === 'latency' || metric === 'ttft') ? 'ms' : metric === 'throughput' ? 'Token/s' : t(`charts:metric_${metric}`)
  const format = (n: number) => formatTrendValue(n, metric, i18n.language)
  const showWindow = !hideWindowFooter && Boolean(resp.window)
  const hasFooter = Boolean(showWindow || ratio || metric === 'throughput')
  return (
    <div className="space-y-3">
      <TimeChart compact={compact} paginateTable={paginateTable} {...chart} percent={ratio} format={format} unit={unit} label={t('charts:usageTrend')} />
      {hasFooter && (
        <div className="flex flex-wrap items-center justify-between gap-x-4 gap-y-1 border-t border-border/50 pt-2.5 text-xs text-muted-foreground tabular-nums">
          {showWindow && resp.window && <p>{resp.window.start_date ?? resp.window.start_at} — {resp.window.end_date ?? resp.window.end_at} · {resp.window.timezone}</p>}
          {ratio && <p>{t('charts:ratioGaps')}</p>}
          {metric === 'throughput' && <p>{t('charts:throughputHint')}</p>}
        </div>
      )}
    </div>
  )
}
