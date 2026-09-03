import { useTranslation } from 'react-i18next'
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
  const models = [...sumByModel(rows).values()].sort((a, b) => b.amount_micro - a.amount_micro)
  const total = models.reduce((s, m) => s + m.amount_micro, 0)

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
      <CardContent className="pt-4">
        <Table>
          <THead>
            <Tr>
              <Th>{t('pricing:model')}</Th>
              <Th className="w-1/3">{t('portal:shareOfSpend')}</Th>
              <Th>{t('common:amount')}</Th>
              <Th>{t('common:requests')}</Th>
              <Th>{t('portal:avgPerCall')}</Th>
              <Th>{t('portal:cacheHitShort')}</Th>
            </Tr>
          </THead>
          <TBody>
            {models.map((m) => {
              const shareBp = total > 0 ? Math.round((m.amount_micro * 10_000) / total) : 0
              const hitBp =
                m.prompt_tokens > 0 ? Math.round((m.cached_tokens * 10_000) / m.prompt_tokens) : 0
              return (
                <Tr key={m.model}>
                  <Td className="font-mono text-xs">{m.model}</Td>
                  <Td>
                    <div className="flex items-center gap-2">
                      <div className="h-2 flex-1 overflow-hidden rounded bg-muted">
                        <div
                          className="h-full bg-primary"
                          style={{ width: `${Math.max(1, shareBp / 100)}%` }}
                        />
                      </div>
                      <span className="w-14 shrink-0 text-right text-xs">
                        {formatBp(shareBp, locale)}
                      </span>
                    </div>
                  </Td>
                  <Td>{formatMoney(m.amount_micro, locale)}</Td>
                  <Td>{formatCount(m.requests, locale)}</Td>
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
