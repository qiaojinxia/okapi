import { useMutation, useQuery } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { Drawer, FieldGroup } from '@/components/ui/drawer'
import { Input, Label } from '@/components/ui/input'
import { Select } from '@/components/ui/select'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatMoney } from '@/lib/money'
import { qk } from '@/lib/query-keys'

/// 套餐抽屉；`initial` 给出即为编辑（plan_code 锁定，后端按 code upsert）。
/// 编辑态回填所需的最小字段（与 PlansPage 的 PlanRow 兼容）。
export interface PlanInitial {
  plan_code: string
  display_name: string
  grant_micro: number
  group_code: string | null
  balance_valid_days: number | null
}

export function PlanDrawer({
  onClose,
  onDone,
  initial,
}: {
  onClose: () => void
  onDone: () => void
  initial?: PlanInitial
}) {
  const { t, i18n } = useTranslation()
  const editing = initial !== undefined
  const [msg, setMsg] = useState<string | null>(null)
  const [form, setForm] = useState({
    plan_code: initial?.plan_code ?? '',
    display_name: initial?.display_name ?? '',
    grant_usd: initial ? String(initial.grant_micro / 1_000_000) : '10',
    group_code: initial?.group_code ?? '',
    balance_valid_days:
      initial?.balance_valid_days === null || initial?.balance_valid_days === undefined
        ? ''
        : String(initial.balance_valid_days),
  })

  // 分组从后端拉，避免手输不存在的 group_code（后端会 400，但太晚）
  const groups = useQuery({
    queryKey: qk.adminGroups,
    queryFn: () => apiFetch<{ data: { group_code: string }[] }>('/admin/groups'),
  })

  const grantMicro = Math.round((Number(form.grant_usd) || 0) * 1_000_000)

  const upsert = useMutation({
    mutationFn: () =>
      apiFetch<{ plan_id: number }>('/admin/plans', {
        method: 'POST',
        body: {
          plan_code: form.plan_code.trim(),
          display_name: form.display_name.trim(),
          grant_micro: grantMicro,
          group_code: form.group_code === '' ? undefined : form.group_code,
          balance_valid_days:
            form.balance_valid_days.trim() === ''
              ? undefined
              : Number(form.balance_valid_days) || undefined,
        },
      }),
    onSuccess: () => {
      onDone()
      onClose()
    },
    onError: (err) => setMsg(describeError(err)),
  })

  return (
    <Drawer
      open
      onClose={onClose}
      title={editing ? t('admin:planEdit', { code: initial.plan_code }) : t('admin:planCreate')}
      description={t('admin:planDrawerDesc')}
      footer={
        <>
          {msg !== null && <span className="mr-auto text-xs text-muted-foreground">{msg}</span>}
          <Button variant="ghost" onClick={onClose}>
            {t('common:cancel')}
          </Button>
          <Button
            disabled={upsert.isPending || form.plan_code.trim() === '' || grantMicro <= 0}
            onClick={() => upsert.mutate()}
          >
            {t('common:save')}
          </Button>
        </>
      }
    >
      <FieldGroup title={t('common:basicInfo')}>
        <div className="grid grid-cols-2 gap-3">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="p-code">{t('admin:planCode')}</Label>
            <Input
              id="p-code"
              className="font-mono text-sm"
              value={form.plan_code}
              placeholder="starter"
              disabled={editing}
              onChange={(e) => setForm((f) => ({ ...f, plan_code: e.target.value }))}
            />
          </div>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="p-name">{t('admin:planName')}</Label>
            <Input
              id="p-name"
              value={form.display_name}
              onChange={(e) => setForm((f) => ({ ...f, display_name: e.target.value }))}
            />
          </div>
        </div>
      </FieldGroup>

      <FieldGroup title={t('admin:planGrant')} hint={t('admin:planGrantHint')}>
        <div className="flex flex-wrap items-end gap-3">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="p-grant">{t('admin:planGrant')}</Label>
            <Input
              id="p-grant"
              className="w-28"
              inputMode="decimal"
              value={form.grant_usd}
              onChange={(e) => setForm((f) => ({ ...f, grant_usd: e.target.value }))}
            />
          </div>
          <span className="pb-2 text-xs text-muted-foreground">
            {t('admin:redeemFaceValue', { amount: formatMoney(grantMicro, i18n.language) })}
          </span>
        </div>
        <div className="flex flex-wrap items-end gap-3">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="p-group">{t('admin:planGroup')}</Label>
            <Select
              id="p-group"
              className="w-44"
              value={form.group_code}
              onChange={(v) => setForm((f) => ({ ...f, group_code: v }))}
              placeholder={t('admin:planGroupKeep')}
              options={(groups.data?.data ?? []).map((g) => ({
                value: g.group_code,
                label: g.group_code,
              }))}
            />
          </div>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="p-days">{t('admin:planValidDays')}</Label>
            <Input
              id="p-days"
              className="w-28"
              inputMode="numeric"
              value={form.balance_valid_days}
              placeholder={t('team:noLimit')}
              onChange={(e) => setForm((f) => ({ ...f, balance_valid_days: e.target.value }))}
            />
          </div>
        </div>
      </FieldGroup>
    </Drawer>
  )
}
