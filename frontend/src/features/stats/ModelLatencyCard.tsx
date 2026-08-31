import { useQuery } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import type { ModelRow } from '@/features/stats/types'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatCount, formatMoney, formatTokensPerSec } from '@/lib/money'
import { qk } from '@/lib/query-keys'

export function ModelLatencyCard({ days }: { days: number }) {
  const { t, i18n } = useTranslation()
  const q = useQuery({
    queryKey: qk.statsModels(days),
    queryFn: () => apiFetch<{ data: ModelRow[] }>(`/admin/stats/models?days=${days}&limit=50`),
  })

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('admin:statModels')}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        <p className="text-xs text-muted-foreground">{t('admin:statModelsHint')}</p>
        {q.isError ? (
          <p className="text-sm text-destructive">{describeError(q.error)}</p>
        ) : (
          <Table>
            <THead>
              <Tr>
                <Th>{t('admin:modelName')}</Th>
                <Th>{t('common:requests')}</Th>
                <Th>{t('common:tokens')}</Th>
                <Th>TTFT p50 / p95 / p99</Th>
                <Th>{t('admin:statLatency')} p50 / p95 / p99</Th>
                <Th>{t('admin:statTps')}</Th>
                <Th>{t('common:amount')}</Th>
              </Tr>
            </THead>
            <TBody>
              {(q.data?.data ?? []).map((m) => (
                <Tr key={m.model}>
                  <Td className="max-w-56 truncate font-mono text-xs">{m.model}</Td>
                  <Td>{formatCount(m.requests, i18n.language)}</Td>
                  <Td>{formatCount(m.tokens, i18n.language)}</Td>
                  <Td className="font-mono text-xs">
                    {m.ttft_p50_ms} / {m.ttft_p95_ms} / {m.ttft_p99_ms} ms
                  </Td>
                  <Td className="font-mono text-xs">
                    {m.latency_p50_ms} / {m.latency_p95_ms} / {m.latency_p99_ms} ms
                  </Td>
                  <Td>{formatTokensPerSec(m.tokens_per_1k_sec, i18n.language)}</Td>
                  <Td>{formatMoney(m.amount_micro, i18n.language)}</Td>
                </Tr>
              ))}
            </TBody>
          </Table>
        )}
      </CardContent>
    </Card>
  )
}
