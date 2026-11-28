import { useQuery } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { Card, CardContent } from '@/components/ui/card'
import { Segmented } from '@/components/ui/segmented'
import { Select } from '@/components/ui/select'
import { EmptyState, ErrorState, LoadingState } from '@/components/ui/state'
import { AnalysisControls, dimensionLabel } from '@/features/analytics/AnalysisControls'
import { FreshnessNotice } from '@/components/freshness-notice'
import { cubeParams } from '@/lib/cube'
import { TrendPlot } from '@/features/analytics/TrendView'
import type { TrendResp } from '@/lib/cube'
import { advancedSearch } from '@/lib/cube'
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
    <div className="flex min-w-0 flex-wrap items-center gap-x-3 gap-y-2">
      <label className="flex min-w-0 max-w-xs items-center gap-2 text-sm">
        <span className="shrink-0 text-muted-foreground">{t('analysis:compare')}</span>
        <Select
          size="sm"
          aria-label={t('analysis:compare')}
          value={search.stack ?? ''}
          onChange={(v) => onChange({ ...search, stack: (v as QualitySearch['stack']) || undefined })}
          options={['', ...QUALITY_COMPARISONS].map((value) => ({
            value,
            label: value ? dimensionLabel(t, value) : t('analytics:stackNone'),
          }))}
        />
      </label>
      <AnalysisControls className="flex-1 basis-64" value={search} onApply={(next) => onChange({ ...search, ...advancedSearch({ ...next }) })} today={query.data?.window?.today} />
    </div>
    <FreshnessNotice value={query.isError ? undefined : query.data?.window?.freshness} />
    {query.isPending ? <LoadingState /> : query.isError ? <ErrorState message={describeError(query.error)} onRetry={() => void query.refetch()} /> : query.data?.data.length ? <TrendPlot key={params} resp={query.data} metric={metric} paginateTable /> : <EmptyState hint={t('admin:trendEmptyHint')} />}
  </CardContent></Card>
}
