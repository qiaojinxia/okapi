import { useMutation, useQuery } from '@tanstack/react-query'
import { createFileRoute } from '@tanstack/react-router'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatMoney } from '@/lib/money'
import { qk } from '@/lib/query-keys'

export const Route = createFileRoute('/admin/')({
  component: AdminOverview,
})

interface DriftRow {
  user_id: number
  events_sum_micro: number
  redis_effective_micro: number
  pg_snapshot_micro: number
}

function AdminOverview() {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const [flushMsg, setFlushMsg] = useState<string | null>(null)

  const recon = useQuery({
    queryKey: qk.reconciliation,
    queryFn: () => apiFetch<{ drift_count: number; drifts: DriftRow[] }>('/admin/reconciliation'),
  })
  const flush = useMutation({
    mutationFn: () => apiFetch('/admin/cache/flush', { method: 'POST', body: {} }),
    onSuccess: () => setFlushMsg(t('common:success')),
    onError: (err) => setFlushMsg(describeError(err)),
  })

  return (
    <div className="flex flex-col gap-4">
      <Card>
        <CardHeader className="flex-row items-center justify-between">
          <CardTitle>{t('admin:reconciliation')}</CardTitle>
          <div className="flex items-center gap-2">
            {flushMsg && <span className="text-xs text-muted-foreground">{flushMsg}</span>}
            <Button size="sm" variant="outline" onClick={() => flush.mutate()}>
              {t('admin:cacheFlush')}
            </Button>
            <Button size="sm" variant="outline" onClick={() => void recon.refetch()}>
              {t('common:refresh')}
            </Button>
          </div>
        </CardHeader>
        <CardContent>
          {recon.isError ? (
            <p className="text-sm text-destructive">{describeError(recon.error)}</p>
          ) : (recon.data?.drifts ?? []).length === 0 ? (
            <p className="text-sm text-success">{t('admin:reconOk')}</p>
          ) : (
            <Table>
              <THead>
                <Tr>
                  <Th>{t('admin:userId')}</Th>
                  <Th>{t('admin:eventsSum')}</Th>
                  <Th>{t('admin:redisEffective')}</Th>
                  <Th>{t('admin:pgSnapshot')}</Th>
                </Tr>
              </THead>
              <TBody>
                {(recon.data?.drifts ?? []).map((d) => (
                  <Tr key={d.user_id}>
                    <Td>{d.user_id}</Td>
                    <Td>{formatMoney(d.events_sum_micro, locale)}</Td>
                    <Td>{formatMoney(d.redis_effective_micro, locale)}</Td>
                    <Td>{formatMoney(d.pg_snapshot_micro, locale)}</Td>
                  </Tr>
                ))}
              </TBody>
            </Table>
          )}
        </CardContent>
      </Card>
    </div>
  )
}
