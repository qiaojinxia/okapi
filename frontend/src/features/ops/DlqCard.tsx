import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import dayjs from 'dayjs'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { toast } from '@/components/ui/toast'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Checkbox } from '@/components/ui/checkbox'
import { useConfirm } from '@/components/ui/confirm'
import { EmptyState, ErrorState, LoadingState } from '@/components/ui/state'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { usePermission } from '@/hooks/use-auth'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatMoney } from '@/lib/money'
import { qk } from '@/lib/query-keys'

interface DlqRow {
  id: number
  source: string
  error: string | null
  retry_count: number
  status: number
  created_at: string
  resolved_at: string | null
  resolved_by: number | null
  request_id: string | null
  user_id: number | null
  model: string | null
  amount_micro: number | null
}

/// 死信队列：账没进统计的那些笔。两种处置——重投（瞬时故障，如 CH 短暂不可达）
/// 与丢弃（毒消息：payload 本身坏的，重投只会再进 DLQ）。
///
/// 多选 + 批量：DLQ 一坏就是一批（同一段 CH 故障期的几十条），逐条点没法用；
/// 但每批都要确认——丢弃是让这些账永久缺席统计，重投则会再写一次 CH。
/// 错误串原样给出：这是判断"能不能重投"的唯一依据，不能摘要。
export function DlqCard() {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const can = usePermission()
  const queryClient = useQueryClient()
  const { confirm, dialog } = useConfirm()
  const [showAll, setShowAll] = useState(false)
  const [selected, setSelected] = useState<Set<number>>(new Set())

  const q = useQuery({
    queryKey: qk.dlq(showAll),
    queryFn: () =>
      apiFetch<{ pending: number; data: DlqRow[] }>(`/admin/dlq?limit=200${showAll ? '&all=true' : ''}`),
  })

  const done = (text: string) => {
    toast.success(text)
    setSelected(new Set())
    void q.refetch()
    // 待办面板与健康芯片的 DLQ 深度同源，一起刷
    void queryClient.invalidateQueries({ queryKey: qk.diagnose })
  }
  const requeue = useMutation({
    mutationFn: (ids: number[]) =>
      apiFetch<{ requeued: number }>('/admin/dlq/requeue', { method: 'POST', body: { ids } }),
    onSuccess: (r) => done(t('admin:dlqRequeued', { n: r.requeued })),
    onError: (err) => toast.error(describeError(err)),
  })
  const discard = useMutation({
    mutationFn: (ids: number[]) =>
      apiFetch<{ discarded: number }>('/admin/dlq/discard', { method: 'POST', body: { ids } }),
    onSuccess: (r) => done(t('admin:dlqDiscarded', { n: r.discarded })),
    onError: (err) => toast.error(describeError(err)),
  })

  const rows = q.data?.data ?? []
  const pendingRows = rows.filter((r) => r.status === 0)
  const ids = [...selected]
  const canWrite = can('billing.refund')
  const toggle = (id: number) =>
    setSelected((s) => {
      const n = new Set(s)
      if (n.has(id)) n.delete(id)
      else n.add(id)
      return n
    })
  const allPendingSelected = pendingRows.length > 0 && pendingRows.every((r) => selected.has(r.id))

  return (
    <Card>
      <CardHeader className="flex-row items-center justify-between">
        <CardTitle>
          {t('admin:dlqTitle')}
          {q.data && (
            <Badge variant={q.data.pending > 0 ? 'destructive' : 'success'} className="ml-2">
              {t('admin:dlqPending', { n: q.data.pending })}
            </Badge>
          )}
        </CardTitle>
        <label className="flex items-center gap-2 text-xs text-muted-foreground">
          <Checkbox checked={showAll} onChange={(v) => setShowAll(v)} />
          {t('admin:dlqShowResolved')}
        </label>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        {dialog}
        <p className="text-xs text-muted-foreground">{t('admin:dlqHint')}</p>

        {canWrite && (
          <div className="flex flex-wrap items-center gap-2">
            <Button
              size="sm"
              variant="outline"
              disabled={ids.length === 0 || requeue.isPending}
              onClick={() =>
                confirm({
                  title: t('admin:dlqRequeue'),
                  description: t('admin:dlqRequeueConfirm', { n: ids.length }),
                  confirmLabel: t('admin:dlqRequeue'),
                  onConfirm: () => requeue.mutate(ids),
                })
              }
            >
              {t('admin:dlqRequeue')} ({ids.length})
            </Button>
            <Button
              size="sm"
              variant="destructive"
              disabled={ids.length === 0 || discard.isPending}
              onClick={() =>
                confirm({
                  title: t('admin:dlqDiscard'),
                  description: t('admin:dlqDiscardConfirm', { n: ids.length }),
                  confirmLabel: t('admin:dlqDiscard'),
                  onConfirm: () => discard.mutate(ids),
                })
              }
            >
              {t('admin:dlqDiscard')} ({ids.length})
            </Button>
          </div>
        )}

        {q.isError ? (
          <ErrorState message={describeError(q.error)} />
        ) : q.isPending ? (
          <LoadingState />
        ) : rows.length === 0 ? (
          <EmptyState hint={t('admin:dlqEmptyHint')} />
        ) : (
          <Table>
            <THead>
              <Tr>
                {canWrite && (
                  <Th className="w-8">
                    <Checkbox
                      checked={allPendingSelected}
                      indeterminate={ids.length > 0 && !allPendingSelected}
                      srLabel={t('admin:dlqSelectAll')}
                      onChange={(v) =>
                        setSelected(v ? new Set(pendingRows.map((r) => r.id)) : new Set())
                      }
                    />
                  </Th>
                )}
                <Th>{t('logs:time')}</Th>
                <Th>{t('common:status')}</Th>
                <Th>{t('admin:dlqSource')}</Th>
                <Th>{t('admin:dlqError')}</Th>
                <Th>{t('admin:logsUser')}</Th>
                <Th>{t('pricing:model')}</Th>
                <Th>{t('common:amount')}</Th>
                <Th>{t('admin:logsRetries')}</Th>
              </Tr>
            </THead>
            <TBody>
              {rows.map((r) => (
                <Tr key={r.id} className={r.status !== 0 ? 'opacity-60' : undefined}>
                  {canWrite && (
                    <Td>
                      {r.status === 0 && (
                        <Checkbox
                          checked={selected.has(r.id)}
                          srLabel={`#${r.id}`}
                          onChange={() => toggle(r.id)}
                        />
                      )}
                    </Td>
                  )}
                  <Td className="whitespace-nowrap text-xs">
                    {dayjs(r.created_at).format('MM-DD HH:mm:ss')}
                  </Td>
                  <Td>
                    {r.status === 0 ? (
                      <Badge variant="destructive">{t('admin:dlqStatusPending')}</Badge>
                    ) : (
                      <Badge variant="muted" title={r.resolved_at ?? ''}>
                        {t('admin:dlqStatusDiscarded')}
                      </Badge>
                    )}
                  </Td>
                  <Td className="font-mono text-xs">{r.source}</Td>
                  {/* 错误原文不截断：判断"能不能重投"的唯一依据 */}
                  <Td className="max-w-md text-xs whitespace-pre-wrap break-all">{r.error ?? '—'}</Td>
                  <Td className="text-xs">{r.user_id === null ? '—' : `#${r.user_id}`}</Td>
                  <Td className="whitespace-nowrap font-mono text-xs">{r.model ?? '—'}</Td>
                  <Td className="whitespace-nowrap text-xs">
                    {r.amount_micro === null ? '—' : formatMoney(r.amount_micro, locale)}
                  </Td>
                  <Td className="text-xs">{r.retry_count}</Td>
                </Tr>
              ))}
            </TBody>
          </Table>
        )}
      </CardContent>
    </Card>
  )
}
