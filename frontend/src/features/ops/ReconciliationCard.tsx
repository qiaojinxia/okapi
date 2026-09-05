import { useMutation, useQuery } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { useConfirm } from '@/components/ui/confirm'
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
///
/// 「按账本校准」是唯一能消掉漂移的动作。对账任务只报不修（此前这里的提示文案却写着
/// "会按账本自动校准"，站长照着等会一直等下去），而热余额丢了不是显示问题——
/// `reserve.lua` 对缺键按 0 处理且不回源 PG，那些用户的 key 是真的调不通。
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

  const { confirm, dialog } = useConfirm()
  const repair = useMutation({
    mutationFn: (body: { user_id: number } | { all: true; limit: number }) =>
      apiFetch<{ repaired: number }>('/admin/reconciliation/repair', { method: 'POST', body }),
    onSuccess: (r) => {
      toast.success(t('admin:reconRepaired', { n: r.repaired }))
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
          {drifts.length > 0 && (
            <Button
              size="sm"
              disabled={repair.isPending}
              onClick={() =>
                confirm({
                  title: t('admin:reconRepairAllTitle'),
                  description: t('admin:reconRepairAllDesc', { n: drifts.length }),
                  confirmLabel: t('admin:reconRepair'),
                  tone: 'default',
                  onConfirm: () => repair.mutate({ all: true, limit: 100000 }),
                })
              }
            >
              {t('admin:reconRepairAll')}
            </Button>
          )}
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
                  <Th />
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
                      <Td>
                        <Button
                          size="xs"
                          variant="outline"
                          disabled={repair.isPending}
                          onClick={() => repair.mutate({ user_id: d.user_id })}
                        >
                          {t('admin:reconRepair')}
                        </Button>
                      </Td>
                    </Tr>
                  )
                })}
              </TBody>
            </Table>
          </>
        )}
      </CardContent>
      {dialog}
    </Card>
  )
}
