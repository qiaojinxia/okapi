import { useTranslation } from 'react-i18next'
import { Link } from '@tanstack/react-router'
import { ArrowUpRight } from 'lucide-react'
import type { PortalLogSearch } from '@/features/logs/search'
import { AutocompleteInput } from '@/components/ui/autocomplete-input'
import { Button } from '@/components/ui/button'
import { Segmented } from '@/components/ui/segmented'
import { chartColor } from '@/lib/chart'
import { usageValue } from './usage-chart-data'
import { Card, CardContent } from '@/components/ui/card'
import { EmptyState } from '@/components/ui/state'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import type { BreakdownRow } from '@/features/portal-overview/types'
import { sumByModel } from '@/features/portal-overview/types'
import { formatBp, formatCount, formatMoney } from '@/lib/money'
import { MODEL_METRICS } from './search'
import type { ModelMetric } from './search'

/// 模型分布（new-api 的"模型消耗分布 + 调用次数占比"两张饼合成一张表）。
///
/// 表而非饼：饼图超过五片就读不出谁是谁，而表能同时给金额占比、请求数、
/// 每次均价——"贵模型用得少但一次很贵" 这种事只有均价列能看出来。
export function ModelShareView({ rows, logSearch, metric, onMetricChange, query, onQueryChange }: {
  rows: BreakdownRow[]; logSearch: PortalLogSearch; metric: ModelMetric
  onMetricChange: (metric: ModelMetric) => void; query: string; onQueryChange: (query: string) => void
}) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const models = [...sumByModel(rows).values()].sort((a, b) => usageValue(b, metric) - usageValue(a, metric) || a.model.localeCompare(b.model))
  const total = models.reduce((s, m) => s + usageValue(m, metric), 0)
  const terms = query.normalize('NFKC').trim().toLowerCase().split(/\s+/).filter(Boolean)
  const visible = models.map((model, index) => ({ ...model, rank: index + 1 })).filter((model) => terms.every((term) => model.model.normalize('NFKC').toLowerCase().includes(term)))

  return (
    <Card className="min-w-0 rounded-xl">
      <CardContent className="space-y-3 px-4 py-3">
        <div className="flex min-w-0 flex-wrap items-center justify-between gap-3">
          <div><h2 className="font-semibold">{t('charts:modelDistribution')}</h2><p className="mt-1 text-xs text-muted-foreground">{t('charts:distributionHint')}</p></div>
          <Segmented ariaLabel={t('charts:distributionMetric')} value={metric} onChange={onMetricChange} options={MODEL_METRICS.map((value) => ({ value, label: t(`charts:metric_${value}`) }))} />
        </div>
        <div className="flex min-w-0 flex-wrap items-center gap-x-4 gap-y-2">
          <AutocompleteInput search className="w-full sm:w-72" inputClassName="h-11 md:h-9" maxLength={256}
            aria-label={t('portal:usedModelSearch')} placeholder={t('portal:usedModelSearchHint')} value={query} onChange={onQueryChange}
            options={models.map((model) => ({ value: model.model }))} emptyHint={t('portal:usedModelNoMatches')} />
          <p className="text-xs text-muted-foreground" aria-live="polite" aria-atomic="true">{t('portal:usedModelCount', { shown: visible.length, total: models.length })}</p>
        </div>
        <p className="text-xs text-muted-foreground">{t('portal:usedModelShareHint')}</p>
        {visible.length === 0 ? <EmptyState hint={terms.length ? t('portal:usedModelNoMatches') : t('portal:emptyUsageHint')}
          action={terms.length > 0 && <Button size="sm" variant="outline" onClick={() => onQueryChange('')}>{t('portal:usedModelClear')}</Button>} /> : <Table stickyHeader stickyFirstColumn aria-label={t('charts:modelDistribution')} wrapperClassName="max-h-[max(12rem,calc(100dvh-32rem))]" scrollResetKey={JSON.stringify([metric, query, logSearch])}>
          <THead>
            <Tr>
              <Th>{t('pricing:model')}</Th>
              <Th className="min-w-44">{t('charts:share')}</Th>
              <Th numeric>{t('common:amount')}</Th>
              <Th numeric>{t('common:requests')}</Th>
              <Th numeric>{t('common:tokens')}</Th>
              <Th numeric>{t('portal:avgPerCall')}</Th>
              <Th numeric>{t('portal:cacheHitShort')}</Th>
            </Tr>
          </THead>
          <TBody>
            {visible.map((m) => {
              const shareBp = total > 0 ? Math.round((usageValue(m, metric) * 10_000) / total) : 0
              const hitBp =
                m.prompt_tokens > 0 ? Math.round((m.cached_tokens * 10_000) / m.prompt_tokens) : 0
              return (
                <Tr key={m.model}>
                  <Td className="w-px text-xs"><span className="flex w-28 items-start gap-2 sm:w-44"><span className="shrink-0 text-muted-foreground">{m.rank}</span><Link to="/portal/logs" search={{ ...logSearch, model: m.model }} title={m.model} aria-label={t('portal:modelLogs', { model: m.model })} className="flex min-w-0 items-start gap-1 rounded font-medium text-primary outline-none hover:underline focus-visible:ring-2 focus-visible:ring-primary/40"><span className="line-clamp-2 break-all">{m.model}</span><ArrowUpRight aria-hidden className="mt-0.5 h-3 w-3 shrink-0" /></Link></span></Td>
                  <Td>
                    <div className="flex items-center gap-2">
                      <div className="h-2 flex-1 overflow-hidden rounded bg-muted">
                        <div
                          className="h-full rounded"
                          style={{ width: `${shareBp / 100}%`, background: chartColor(m.rank - 1) }}
                        />
                      </div>
                      <span className="w-14 shrink-0 text-right text-xs">
                        {total > 0 ? formatBp(shareBp, locale) : '—'}
                      </span>
                    </div>
                  </Td>
                  <Td numeric className="whitespace-nowrap">{formatMoney(m.amount_micro, locale)}</Td>
                  <Td numeric className="whitespace-nowrap">{formatCount(m.requests, locale)}</Td>
                  <Td numeric className="whitespace-nowrap">{formatCount(m.prompt_tokens + m.completion_tokens, locale)}</Td>
                  <Td numeric className="whitespace-nowrap text-xs">
                    {m.requests > 0 ? formatMoney(Math.round(m.amount_micro / m.requests), locale) : '—'}
                  </Td>
                  <Td numeric className="whitespace-nowrap text-xs">{m.prompt_tokens > 0 ? formatBp(hitBp, locale) : '—'}</Td>
                </Tr>
              )
            })}
          </TBody>
        </Table>}
      </CardContent>
    </Card>
  )
}
