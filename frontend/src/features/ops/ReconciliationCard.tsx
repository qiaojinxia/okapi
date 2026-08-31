import { useMutation, useQuery } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { ErrorState } from '@/components/ui/state'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatMoney } from '@/lib/money'
import { qk } from '@/lib/query-keys'

export interface DriftRow {
  user_id: number
  events_sum_micro: number
  redis_effective_micro: number
  pg_snapshot_micro: number
}

export interface ReconResp {
  drift_count: number
  drifts: DriftRow[]
}

/// 三方对账（事件重放 / Redis 热账本 / PG 快照）。
///
/// 从落地页移到运维页：这是排障工具而非日常动线——无漂移时它只能显示"一切正常"，
/// 占着首屏最贵的位置；有漂移时又需要在这里连带做缓存刷新等处置动作。
export function ReconciliationCard() {
  const { t, i18n } = useTranslation()
  const [msg, setMsg] = useState<string | null>(null)

  const recon = useQuery({
    queryKey: qk.reconciliation,
    queryFn: () => apiFetch<ReconResp>('/admin/reconciliation'),
  })
  const flush = useMutation({
    mutationFn: () => apiFetch('/admin/cache/flush', { method: 'POST', body: {} }),
    onSuccess: () => setMsg(t('common:success')),
    onError: (err) => setMsg(describeError(err)),
  })

  const drifts = recon.data?.drifts ?? []

  return (
    <Card>
      <CardHeader className="flex-row items-center justify-between">
        <CardTitle>{t('admin:reconciliation')}</CardTitle>
        <div className="flex items-center gap-2">
          {msg !== null && <span className="text-xs text-muted-foreground">{msg}</span>}
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
          <ErrorState message={describeError(recon.error)} />
        ) : drifts.length === 0 ? (
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
              {drifts.map((d) => (
                <Tr key={d.user_id}>
                  <Td>{d.user_id}</Td>
                  <Td>{formatMoney(d.events_sum_micro, i18n.language)}</Td>
                  <Td>{formatMoney(d.redis_effective_micro, i18n.language)}</Td>
                  <Td>{formatMoney(d.pg_snapshot_micro, i18n.language)}</Td>
                </Tr>
              ))}
            </TBody>
          </Table>
        )}
      </CardContent>
    </Card>
  )
}
