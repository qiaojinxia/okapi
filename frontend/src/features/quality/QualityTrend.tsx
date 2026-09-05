import { useQuery } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Card, CardContent } from '@/components/ui/card'
import { Segmented } from '@/components/ui/segmented'
import { EmptyState, ErrorState, LoadingState } from '@/components/ui/state'
import type { AnalyticsSearch, StackDim } from '@/routes/admin.stats'
import { AnalysisControls, dimensionLabel, selectClass } from '@/features/analytics/AnalysisControls'
import { FreshnessNotice } from '@/features/analytics/FreshnessNotice'
import { cubeParams } from '@/features/analytics/search'
import { TrendPlot } from '@/features/analytics/TrendView'
import type { TrendResp } from '@/features/analytics/types'
import type { TrendMetric } from '@/features/analytics/trend-data'
import { apiFetch } from '@/lib/api'
import { qk } from '@/lib/query-keys'
import { describeError } from '@/lib/i18n'

export function QualityTrend({ days }: { days: number }) {
  const { t } = useTranslation()
  const [metric, setMetric] = useState<TrendMetric>('error_rate')
  const [filters, setFilters] = useState<AnalyticsSearch>({})
  const [stack, setStack] = useState<StackDim | undefined>()
  const params = cubeParams({ ...filters, days }, { stack, metric })
  const query = useQuery({ queryKey: qk.statsTrend(params), queryFn: () => apiFetch<TrendResp>(`/admin/stats/trend?${params}`), retry: false })
  return <Card><CardContent className="space-y-4 pt-5"><div className="flex flex-wrap items-center justify-between gap-3"><h2 className="font-semibold">{t('charts:qualityTrend')}</h2><Segmented ariaLabel={t('charts:metric')} value={metric} onChange={setMetric} options={(['error_rate', 'latency', 'ttft', 'throughput'] as const).map((value) => ({ value, label: t(`charts:metric_${value}`) }))} /></div>
    <AnalysisControls value={{ ...filters, days }} onApply={setFilters} today={query.data?.window?.today} />
    <label className="flex max-w-sm items-center gap-2 text-sm"><span className="shrink-0">{t('analysis:compare')}</span><select className={selectClass} value={stack ?? ''} onChange={(e) => setStack(e.target.value as StackDim || undefined)}>{['', 'model', 'model_group', 'group', 'channel', 'node'].map((v) => <option key={v} value={v}>{v ? dimensionLabel(t, v) : t('analytics:stackNone')}</option>)}</select></label>
    <FreshnessNotice value={query.data?.window?.freshness} />
    {query.isPending ? <LoadingState /> : query.isError ? <ErrorState message={describeError(query.error)} onRetry={() => void query.refetch()} /> : query.data?.data.length ? <TrendPlot key={metric} resp={query.data} metric={metric} /> : <EmptyState hint={t('admin:trendEmptyHint')} />}
  </CardContent></Card>
}
