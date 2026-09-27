import { useQuery } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { Card, CardContent } from '@/components/ui/card'
import { Segmented } from '@/components/ui/segmented'
import { EmptyState, ErrorState, LoadingState } from '@/components/ui/state'
import { AnalysisControls, dimensionLabel, selectClass } from '@/features/analytics/AnalysisControls'
import { FreshnessNotice } from '@/features/analytics/FreshnessNotice'
import { cubeParams } from '@/features/analytics/search'
import { TrendPlot } from '@/features/analytics/TrendView'
import type { TrendResp } from '@/features/analytics/types'
import { advancedSearch } from '@/features/analytics/advanced-search'
import { QUALITY_COMPARISONS, QUALITY_METRICS } from './search'
import type { QualitySearch } from './search'
import { apiFetch } from '@/lib/api'
import { qk } from '@/lib/query-keys'
import { describeError } from '@/lib/i18n'

export function QualityTrend({ search, onChange }: { search: QualitySearch; onChange: (next: QualitySearch) => void }) {
  const { t } = useTranslation()
  const metric = search.measure ?? 'error_rate'
  const params = cubeParams(search, { stack: search.stack, metric })
  const query = useQuery({ queryKey: qk.statsTrend(params), queryFn: () => apiFetch<TrendResp>(`/admin/stats/trend?${params}`), retry: false })
  return <Card className="min-w-0 rounded-xl"><CardContent className="space-y-3 px-4 py-3"><div className="flex min-w-0 flex-wrap items-center justify-between gap-3"><h2 className="font-semibold">{t('charts:qualityTrend')}</h2><Segmented ariaLabel={t('charts:metric')} value={metric} onChange={(measure) => onChange({ ...search, measure })} options={QUALITY_METRICS.map((value) => ({ value, label: t(`charts:metric_${value}`) }))} /></div>
    <AnalysisControls value={search} onApply={(next) => onChange({ ...search, ...advancedSearch({ ...next }) })} today={query.data?.window?.today} />
    <label className="flex min-w-0 max-w-sm items-center gap-2 text-sm"><span className="shrink-0">{t('analysis:compare')}</span><select className={selectClass} value={search.stack ?? ''} onChange={(e) => onChange({ ...search, stack: e.target.value as QualitySearch['stack'] || undefined })}>{['', ...QUALITY_COMPARISONS].map((value) => <option key={value} value={value}>{value ? dimensionLabel(t, value) : t('analytics:stackNone')}</option>)}</select></label>
    <FreshnessNotice value={query.isError ? undefined : query.data?.window?.freshness} />
    {query.isPending ? <LoadingState /> : query.isError ? <ErrorState message={describeError(query.error)} onRetry={() => void query.refetch()} /> : query.data?.data.length ? <TrendPlot key={`${metric}-${query.data.stack ?? 'none'}`} resp={query.data} metric={metric} /> : <EmptyState hint={t('admin:trendEmptyHint')} />}
  </CardContent></Card>
}
