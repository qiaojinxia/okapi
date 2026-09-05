import { useTranslation } from 'react-i18next'
import { useState } from 'react'
import { Segmented } from '@/components/ui/segmented'
import { chartColor } from '@/lib/chart'
import { usageValue } from './usage-chart-data'
import { Card, CardContent } from '@/components/ui/card'
import { EmptyState } from '@/components/ui/state'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import type { BreakdownRow } from '@/features/portal-overview/types'
import { sumByModel } from '@/features/portal-overview/types'
import { formatBp, formatCount, formatMoney } from '@/lib/money'

/// 模型分布（new-api 的"模型消耗分布 + 调用次数占比"两张饼合成一张表）。
///
/// 表而非饼：饼图超过五片就读不出谁是谁，而表能同时给金额占比、请求数、
/// 每次均价——"贵模型用得少但一次很贵" 这种事只有均价列能看出来。
export function ModelShareView({ rows }: { rows: BreakdownRow[] }) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const [metric, setMetric] = useState<'amount' | 'requests' | 'tokens'>('amount')
  const models = [...sumByModel(rows).values()].sort((a, b) => usageValue(b, metric) - usageValue(a, metric))
  const total = models.reduce((s, m) => s + usageValue(m, metric), 0)

  if (models.length === 0) {
    return (
      <Card>
        <CardContent>
          <EmptyState hint={t('portal:emptyUsageHint')} />
        </CardContent>
      </Card>
    )
  }
  return (
    <Card>
      <CardContent className="space-y-4 pt-5">
        <div className="flex flex-wrap items-center justify-between gap-3"><div><h2 className="font-semibold">{t('charts:modelDistribution')}</h2><p className="mt-1 text-xs text-muted-foreground">{t('charts:distributionHint')}</p></div><Segmented ariaLabel={t('charts:distributionMetric')} value={metric} onChange={setMetric} options={(['amount', 'requests', 'tokens'] as const).map((value) => ({ value, label: t(`charts:metric_${value}`) }))} /></div>
        <Table>
          <THead>
            <Tr>
              <Th>{t('pricing:model')}</Th>
              <Th className="min-w-44">{t('charts:share')}</Th>
              <Th>{t('common:amount')}</Th>
              <Th>{t('common:requests')}</Th>
              <Th>{t('common:tokens')}</Th>
              <Th>{t('portal:avgPerCall')}</Th>
              <Th>{t('portal:cacheHitShort')}</Th>
            </Tr>
          </THead>
          <TBody>
            {models.map((m, index) => {
              const shareBp = total > 0 ? Math.round((usageValue(m, metric) * 10_000) / total) : 0
              const hitBp =
                m.prompt_tokens > 0 ? Math.round((m.cached_tokens * 10_000) / m.prompt_tokens) : 0
              return (
                <Tr key={m.model}>
                  <Td className="max-w-64 text-xs"><span className="mr-2 text-muted-foreground">{index + 1}</span><span className="break-all font-medium">{m.model}</span></Td>
                  <Td>
                    <div className="flex items-center gap-2">
                      <div className="h-2 flex-1 overflow-hidden rounded bg-muted">
                        <div
                          className="h-full rounded"
                          style={{ width: `${shareBp / 100}%`, background: chartColor(index) }}
                        />
                      </div>
                      <span className="w-14 shrink-0 text-right text-xs">
                        {formatBp(shareBp, locale)}
                      </span>
                    </div>
                  </Td>
                  <Td>{formatMoney(m.amount_micro, locale)}</Td>
                  <Td>{formatCount(m.requests, locale)}</Td>
                  <Td>{formatCount(m.prompt_tokens + m.completion_tokens, locale)}</Td>
                  <Td className="text-xs">
                    {m.requests > 0 ? formatMoney(Math.round(m.amount_micro / m.requests), locale) : '—'}
                  </Td>
                  <Td className="text-xs">{formatBp(hitBp, locale)}</Td>
                </Tr>
              )
            })}
          </TBody>
        </Table>
      </CardContent>
    </Card>
  )
}
