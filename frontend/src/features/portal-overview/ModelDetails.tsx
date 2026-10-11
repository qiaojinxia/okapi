import { useTranslation } from 'react-i18next'
import type { PortalLogSearch } from '@/features/logs/search'
import { Card, CardContent } from '@/components/ui/card'
import { Segmented } from '@/components/ui/segmented'
import { ModelShareView } from './ModelShareView'
import { TokenMixView } from './TokenMixView'
import type { ModelMetric } from './search'
import type { BreakdownRow, BreakdownTotal } from './types'

export type DetailMode = 'models' | 'tokens'

/// 按模型一行的明细表：「费用」看金额、请求、均价与占比，「Token」看输入、缓存读写、输出、推理。
/// 两种都是同一份用量响应换一组列，切换不额外查询。
export function ModelDetails({ rows, total, logSearch, mode, onModeChange, metric, onMetricChange, query, onQueryChange }: {
  rows: BreakdownRow[]; total: BreakdownTotal | null; logSearch: PortalLogSearch
  mode: DetailMode; onModeChange: (mode: DetailMode) => void
  metric: ModelMetric; onMetricChange: (metric: ModelMetric) => void
  query: string; onQueryChange: (query: string) => void
}) {
  const { t } = useTranslation()
  return (
    <Card id="portal-details" className="min-w-0 scroll-mt-4 rounded-xl">
      <CardContent className="space-y-3 px-4 py-3">
        <section aria-label={t('portal:modelDetails')} className="space-y-3">
          <div className="flex min-w-0 flex-wrap items-center justify-between gap-3">
            <h2 className="font-semibold">{t('portal:modelDetails')}</h2>
            <Segmented ariaLabel={t('portal:detailMode')} size="sm" value={mode} onChange={onModeChange}
              options={[{ value: 'models', label: t('portal:detailSpend') }, { value: 'tokens', label: t('portal:detailTokens') }]} />
          </div>
          {mode === 'models'
            ? <ModelShareView rows={rows} logSearch={logSearch} metric={metric} onMetricChange={onMetricChange} query={query} onQueryChange={onQueryChange} />
            : <TokenMixView rows={rows} total={total} />}
        </section>
      </CardContent>
    </Card>
  )
}
