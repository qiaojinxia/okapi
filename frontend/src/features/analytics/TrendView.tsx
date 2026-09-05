import { useNavigate } from '@tanstack/react-router'
import { useTranslation } from 'react-i18next'
import type { AnalyticsSearch, StackDim } from '@/routes/admin.stats'
import { STACK_DIMS } from '@/routes/admin.stats'
import { Card, CardContent } from '@/components/ui/card'
import { Segmented } from '@/components/ui/segmented'
import { EmptyState, ErrorState, LoadingState } from '@/components/ui/state'
import { TimeChart } from '@/components/ui/time-chart'
import { cleanSearch } from './search'
import type { TrendResp } from './types'
import { STACKABLE, TREND_METRICS, trendChart } from './trend-data'
import type { TrendMetric } from './trend-data'
import { dimensionLabel } from './AnalysisControls'
import { describeError } from '@/lib/i18n'

export function TrendView({ search, resp, isLoading, error }: { search: AnalyticsSearch; resp: TrendResp | undefined; isLoading: boolean; error: unknown }) {
  const { t } = useTranslation()
  const navigate = useNavigate({ from: '/admin/stats' })
  const metric: TrendMetric = search.measure ?? 'amount'
  const setStack = (v: StackDim | 'none') => void navigate({ search: (prev) => cleanSearch({ ...prev, stack: v === 'none' ? undefined : v }) })
  const pickMetric = (measure: TrendMetric) => void navigate({ search: (prev) => cleanSearch({ ...prev, measure }) })
  return <Card><CardContent className="space-y-4 pt-5">
    <div className="flex flex-wrap items-center justify-between gap-3">
      <Segmented ariaLabel={t('charts:metric')} options={TREND_METRICS.map((value) => ({ value, label: t(`charts:metric_${value}`) }))} value={metric} onChange={pickMetric} size="sm" />
      <label className="flex min-h-9 items-center gap-2 text-xs text-muted-foreground">{t(STACKABLE.includes(metric) ? 'analytics:stackBy' : 'analysis:compare')}<select aria-label={t(STACKABLE.includes(metric) ? 'analytics:stackBy' : 'analysis:compare')} value={search.stack ?? 'none'} onChange={(e) => setStack(e.target.value as StackDim | 'none')} className="h-9 max-w-full rounded-lg border border-border bg-card px-2 text-foreground">{(['none', ...STACK_DIMS] as const).map((value) => <option key={value} value={value}>{value === 'none' ? t('analytics:stackNone') : dimensionLabel(t, value)}</option>)}</select></label>
    </div>
    {error != null ? <ErrorState message={describeError(error)} /> : isLoading || !resp ? <LoadingState /> : !resp.data.length ? <EmptyState hint={t('admin:trendEmptyHint')} /> : <TrendPlot key={`${metric}-${resp.stack ?? 'none'}`} resp={resp} metric={metric} />}
  </CardContent></Card>
}

export function TrendPlot({ resp, metric }: { resp: TrendResp; metric: TrendMetric }) {
  const { t, i18n } = useTranslation()
  const chart = trendChart(resp, metric, t(`charts:metric_${metric}`), t('analytics:ttft'), t('analytics:other'), t('analysis:notCollected'))
  const ratio = metric === 'cache' || metric === 'error_rate'
  const unit = metric === 'amount' ? 'USD' : ratio ? '%' : (metric === 'latency' || metric === 'ttft') ? 'ms' : metric === 'throughput' ? 'Token/s' : t(`charts:metric_${metric}`)
  const format = (n: number) => metric === 'amount' ? new Intl.NumberFormat(i18n.language, { style: 'currency', currency: 'USD', maximumFractionDigits: 4 }).format(n)
    : `${n.toLocaleString(i18n.language, { maximumFractionDigits: ratio || metric === 'throughput' ? 2 : 0 })}${ratio ? '%' : (metric === 'latency' || metric === 'ttft') ? ' ms' : metric === 'throughput' ? ' Token/s' : ''}`
  return <div className="space-y-3"><TimeChart {...chart} percent={ratio} format={format} unit={unit} label={t('charts:usageTrend')} />{resp.window && <p className="text-xs text-muted-foreground">{resp.window.start_date ?? resp.window.start_at} — {resp.window.end_date ?? resp.window.end_at} · {resp.window.timezone}</p>}{ratio && <p className="text-xs text-muted-foreground">{t('charts:ratioGaps')}</p>}{metric === 'throughput' && <p className="text-xs text-muted-foreground">{t('charts:throughputHint')}</p>}</div>
}
