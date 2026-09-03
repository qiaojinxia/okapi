import { useMutation, useQuery } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { toast } from '@/components/ui/toast'
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
  username: string | null
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
/// 展示用管理员语言而非架构黑话：三列钱各自"是什么、谁说了算"要一句话讲清，
/// 差额直接算好标红——此前只给三列原始数字，管理员对着 events_sum 不知道
/// 该信哪列、差多少、下一步干什么。
export function ReconciliationCard() {
  const { t, i18n } = useTranslation()

  const recon = useQuery({
    queryKey: qk.reconciliation,
    queryFn: () => apiFetch<ReconResp>('/admin/reconciliation'),
  })
  const flush = useMutation({
    mutationFn: () => apiFetch('/admin/cache/flush', { method: 'POST', body: {} }),
    onSuccess: () => {
      toast.success(t('admin:reconFlushed'))
      void recon.refetch()
    },
    onError: (err) => toast.error(describeError(err)),
  })

  const drifts = recon.data?.drifts ?? []

  return (
    <Card>
      <CardHeader className="flex-row items-center justify-between">
        <CardTitle>{t('admin:reconciliation')}</CardTitle>
        <div className="flex items-center gap-2">
          <Button size="sm" variant="outline" onClick={() => flush.mutate()}>
            {t('admin:cacheFlush')}
          </Button>
          <Button size="sm" variant="outline" onClick={() => void recon.refetch()}>
            {t('common:refresh')}
          </Button>
        </div>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        {/* 三列钱各自是什么、以谁为准：不解释清楚，出漂移时管理员不知道该信哪列 */}
        <p className="text-xs text-muted-foreground">{t('admin:reconLegend')}</p>
        {recon.isError ? (
          <ErrorState message={describeError(recon.error)} />
        ) : drifts.length === 0 ? (
          <p className="text-sm text-success">{t('admin:reconOk')}</p>
        ) : (
          <>
            <p className="text-xs text-warning">{t('admin:reconDriftHint')}</p>
            <Table>
              <THead>
                <Tr>
                  <Th>{t('admin:username')}</Th>
                  <Th>{t('admin:reconLedger')}</Th>
                  <Th>{t('admin:reconLive')}</Th>
                  <Th>{t('admin:reconDelta')}</Th>
                  <Th>{t('admin:reconSnapshot')}</Th>
                </Tr>
              </THead>
              <TBody>
                {drifts.map((d) => {
                  const delta = d.redis_effective_micro - d.events_sum_micro
                  return (
                    <Tr key={d.user_id}>
                      <Td>
                        {d.username ?? '?'}{' '}
                        <span className="text-xs text-muted-foreground">#{d.user_id}</span>
                      </Td>
                      <Td>{formatMoney(d.events_sum_micro, i18n.language)}</Td>
                      <Td>{formatMoney(d.redis_effective_micro, i18n.language)}</Td>
                      <Td>
                        <Badge variant={delta === 0 ? 'muted' : 'destructive'}>
                          {delta > 0 ? '+' : ''}
                          {formatMoney(delta, i18n.language)}
                        </Badge>
                      </Td>
                      <Td className="text-muted-foreground">
                        {formatMoney(d.pg_snapshot_micro, i18n.language)}
                      </Td>
                    </Tr>
                  )
                })}
              </TBody>
            </Table>
          </>
        )}
      </CardContent>
    </Card>
  )
}
