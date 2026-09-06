import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import dayjs from 'dayjs'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { useConfirm } from '@/components/ui/confirm'
import { FieldGroup } from '@/components/ui/drawer'
import { Label } from '@/components/ui/input'
import { Select } from '@/components/ui/select'
import { TableSkeleton } from '@/components/ui/skeleton'
import { ErrorState } from '@/components/ui/state'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { toast } from '@/components/ui/toast'
import type { MySubscription } from '@/features/subscriptions/PortalPlansPage'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatMoney } from '@/lib/money'
import { qk } from '@/lib/query-keys'

interface PlanOption {
  plan_code: string
  display_name: string
  kind: 0 | 1
  status: number
}

/// 用户抽屉"订阅"签（IMPLEMENTATION §11.28）：当前订阅 + 发放 / 续期 + 立即结束 + 历史。
///
/// 发放走 `/admin/users/{id}/subscription`（免费，与调余额同一权限点）；同套餐再发即续期，
/// 异套餐后端 409——下拉里仍列出以便看清有哪些套餐，被拒时由错误文案解释。
export function SubscriptionSection({ userId, onDone }: { userId: number; onDone: () => void }) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const queryClient = useQueryClient()
  const { confirm, dialog } = useConfirm()
  const [planCode, setPlanCode] = useState('')

  const sub = useQuery({
    queryKey: qk.userSubscription(userId),
    queryFn: () => apiFetch<MySubscription>(`/admin/users/${userId}/subscription`),
  })
  const plans = useQuery({
    queryKey: [...qk.adminPlans, 'options'],
    queryFn: () => apiFetch<{ data: PlanOption[] }>('/admin/plans?limit=200'),
  })
  const options = (plans.data?.data ?? []).filter((p) => p.kind === 1 && p.status === 1)

  const refresh = () => {
    void queryClient.invalidateQueries({ queryKey: qk.userSubscription(userId) })
    onDone()
  }
  const grant = useMutation({
    mutationFn: () =>
      apiFetch<{ outcome: string }>(`/admin/users/${userId}/subscription`, {
        method: 'POST',
        body: { plan_code: planCode },
      }),
    onSuccess: (r) => {
      toast.success(r.outcome === 'renewed' ? t('portal:subRenewed') : t('portal:subActivated'))
      refresh()
    },
    onError: (err) => toast.error(describeError(err)),
  })
  const cancel = useMutation({
    mutationFn: () => apiFetch(`/admin/users/${userId}/subscription`, { method: 'DELETE' }),
    onSuccess: () => {
      toast.success(t('common:success'))
      refresh()
    },
    onError: (err) => toast.error(describeError(err)),
  })

  const current = sub.data?.subscription ?? null
  const fmt = (iso: string) => dayjs(iso).format('YYYY-MM-DD HH:mm')

  return (
    <>
      {dialog}
      <FieldGroup title={t('portal:subCurrentTitle')}>
        {sub.isError ? (
          <ErrorState message={describeError(sub.error)} />
        ) : sub.isPending ? (
          <TableSkeleton rows={2} cols={3} />
        ) : current === null ? (
          <p className="text-sm text-muted-foreground">{t('admin:userSubNone')}</p>
        ) : (
          <div className="flex flex-col gap-2 rounded-md border border-border p-3 text-sm">
            <div className="flex flex-wrap items-center gap-2">
              <span className="font-medium">{current.display_name}</span>
              <Badge variant="muted" className="font-mono">{current.plan_code}</Badge>
              <Badge variant="success">{t('portal:subStatus_1')}</Badge>
              {current.group_code !== null && (
                <Badge variant={current.granted_group ? 'info' : 'muted'}>
                  {t('portal:planWithGroup', { group: current.group_code })}
                </Badge>
              )}
            </div>
            <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-xs">
              <dt className="text-muted-foreground">{t('portal:subRemaining')}</dt>
              <dd className="tabular-nums">
                {formatMoney(current.remaining_micro, locale)} / {formatMoney(current.quota_micro, locale)}
              </dd>
              <dt className="text-muted-foreground">{t('admin:planPeriod')}</dt>
              <dd>
                {t(`admin:planPeriod_${current.period}`)} · {t('portal:subResetsAt', { at: fmt(current.window_end) })}
              </dd>
              <dt className="text-muted-foreground">{t('admin:userSubExpires')}</dt>
              <dd>{fmt(current.expires_at)}</dd>
              <dt className="text-muted-foreground">{t('portal:ledgerSource')}</dt>
              <dd className="font-mono">{current.source}</dd>
            </dl>
            <div>
              <Button
                size="sm"
                variant="destructive"
                loading={cancel.isPending}
                onClick={() =>
                  confirm({
                    title: t('admin:userSubCancel'),
                    description: t('admin:userSubCancelConfirm'),
                    confirmLabel: t('admin:userSubCancel'),
                    onConfirm: () => cancel.mutate(),
                  })
                }
              >
                {t('admin:userSubCancel')}
              </Button>
            </div>
          </div>
        )}
      </FieldGroup>

      <FieldGroup title={t('admin:userSubGrant')} hint={t('admin:userSubGrantHint')}>
        <div className="flex flex-wrap items-end gap-3">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="sub-plan">{t('admin:planKind_1')}</Label>
            <Select
              id="sub-plan"
              className="w-56"
              value={planCode}
              onChange={setPlanCode}
              placeholder={t('admin:planCode')}
              options={options.map((p) => ({
                value: p.plan_code,
                label: `${p.display_name} (${p.plan_code})`,
              }))}
            />
          </div>
          <Button size="sm" disabled={planCode === ''} loading={grant.isPending} onClick={() => grant.mutate()}>
            {current?.plan_code === planCode ? t('portal:planRenew') : t('admin:userSubGrant')}
          </Button>
        </div>
      </FieldGroup>

      {sub.data !== undefined && sub.data.history.length > 0 && (
        <FieldGroup title={t('admin:userSubHistory')}>
          <Table>
            <THead>
              <Tr>
                <Th>{t('admin:planCode')}</Th>
                <Th>{t('common:status')}</Th>
                <Th numeric>{t('admin:planQuota')}</Th>
                <Th>{t('admin:userSubValidity')}</Th>
              </Tr>
            </THead>
            <TBody>
              {sub.data.history.map((h) => (
                <Tr key={h.id}>
                  <Td className="font-mono text-xs">{h.plan_code}</Td>
                  <Td>
                    <Badge variant={h.status === 1 ? 'success' : 'muted'}>{t(`portal:subStatus_${h.status}`)}</Badge>
                  </Td>
                  <Td numeric>{formatMoney(h.quota_micro, locale)}</Td>
                  <Td className="text-xs">
                    {fmt(h.starts_at)} → {fmt(h.expires_at)}
                  </Td>
                </Tr>
              ))}
            </TBody>
          </Table>
        </FieldGroup>
      )}
    </>
  )
}
