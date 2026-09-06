import { useMutation, useQuery } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { Drawer, FieldGroup } from '@/components/ui/drawer'
import { Input, Label } from '@/components/ui/input'
import { Segmented } from '@/components/ui/segmented'
import { Select } from '@/components/ui/select'
import { Textarea } from '@/components/ui/textarea'
import { toast } from '@/components/ui/toast'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatMoney } from '@/lib/money'
import { qk } from '@/lib/query-keys'

/// 套餐抽屉；`initial` 给出即为编辑（plan_code 锁定，后端按 code upsert）。
/// 编辑态回填所需的最小字段（与 PlansPage 的 PlanRow 兼容）。
export interface PlanInitial {
  plan_code: string
  display_name: string
  /// 0 充值模板 / 1 订阅（IMPLEMENTATION §11.28）。
  kind: 0 | 1
  grant_micro: number
  group_code: string | null
  balance_valid_days: number | null
  price_micro: number
  period: 1 | 2 | 3 | null
  duration_days: number | null
  sort_order: number
  description: string | null
}

const PERIODS = [1, 2, 3] as const

function usdInput(micro: number): string {
  return String(micro / 1_000_000)
}

function toMicro(usd: string): number {
  return Math.round((Number(usd) || 0) * 1_000_000)
}

function intOrUndef(raw: string): number | undefined {
  const n = Number(raw.trim())
  return raw.trim() === '' || !Number.isFinite(n) || n <= 0 ? undefined : Math.trunc(n)
}

/// 两种形态共用一张表：kind 决定"入账一次"还是"每窗重置"。切换时字段区随之替换，
/// 而不是把八个字段一起摊开——运营建充值模板时不该看到"周期"这种词。
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
  const [kind, setKind] = useState<0 | 1>(initial?.kind ?? 0)
  const [form, setForm] = useState({
    plan_code: initial?.plan_code ?? '',
    display_name: initial?.display_name ?? '',
    grant_usd: initial ? usdInput(initial.grant_micro) : '10',
    group_code: initial?.group_code ?? '',
    balance_valid_days: initial?.balance_valid_days == null ? '' : String(initial.balance_valid_days),
    price_usd: initial && initial.price_micro > 0 ? usdInput(initial.price_micro) : '',
    period: initial?.period ?? 3,
    duration_days: initial?.duration_days == null ? '30' : String(initial.duration_days),
    sort_order: String(initial?.sort_order ?? 0),
    description: initial?.description ?? '',
  })

  // 分组从后端拉，避免手输不存在的 group_code（后端会 400，但太晚）
  const groups = useQuery({
    queryKey: qk.adminGroups,
    queryFn: () => apiFetch<{ data: { group_code: string }[] }>('/admin/groups'),
  })

  const grantMicro = toMicro(form.grant_usd)
  const priceMicro = form.price_usd.trim() === '' ? 0 : toMicro(form.price_usd)
  const durationDays = intOrUndef(form.duration_days)
  const subscriptionInvalid = kind === 1 && (durationDays === undefined || priceMicro < 0)

  const upsert = useMutation({
    mutationFn: () =>
      apiFetch<{ plan_id: number }>('/admin/plans', {
        method: 'POST',
        body: {
          plan_code: form.plan_code.trim(),
          display_name: form.display_name.trim(),
          kind,
          grant_micro: grantMicro,
          group_code: form.group_code === '' ? undefined : form.group_code,
          balance_valid_days: kind === 0 ? intOrUndef(form.balance_valid_days) : undefined,
          price_micro: kind === 1 ? priceMicro : 0,
          period: kind === 1 ? form.period : undefined,
          duration_days: kind === 1 ? durationDays : undefined,
          sort_order: Math.trunc(Number(form.sort_order) || 0),
          description: form.description.trim() === '' ? undefined : form.description.trim(),
        },
      }),
    onSuccess: () => {
      onDone()
      onClose()
    },
    onError: (err) => toast.error(describeError(err)),
  })

  const money = (micro: number) => formatMoney(micro, i18n.language)
  const periodLabel = (p: 1 | 2 | 3) => t(`admin:planPeriod_${p}`)

  return (
    <Drawer
      open
      onClose={onClose}
      title={editing ? t('admin:planEdit', { code: initial.plan_code }) : t('admin:planCreate')}
      description={t('admin:planDrawerDesc')}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>
            {t('common:cancel')}
          </Button>
          <Button
            disabled={
              upsert.isPending || form.plan_code.trim() === '' || grantMicro <= 0 || subscriptionInvalid
            }
            onClick={() => upsert.mutate()}
          >
            {t('common:save')}
          </Button>
        </>
      }
    >
      <FieldGroup title={t('common:basicInfo')}>
        <div className="flex flex-col gap-1.5">
          <Label>{t('admin:planKind')}</Label>
          <Segmented
            className="h-9 w-full [&>button]:flex-1"
            ariaLabel={t('admin:planKind')}
            value={kind}
            onChange={setKind}
            options={[
              { value: 0, label: t('admin:planKind_0') },
              { value: 1, label: t('admin:planKind_1') },
            ]}
          />
          <p className="text-xs text-muted-foreground">
            {kind === 1 ? t('admin:planKindSubHint') : t('admin:planKindTopupHint')}
          </p>
        </div>
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

      {kind === 0 ? (
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
              {t('admin:redeemFaceValue', { amount: money(grantMicro) })}
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
      ) : (
        <>
          <FieldGroup title={t('admin:planQuota')} hint={t('admin:planQuotaHint')}>
            <div className="flex flex-wrap items-end gap-3">
              <div className="flex flex-col gap-1.5">
                <Label htmlFor="p-quota">{t('admin:planQuota')}</Label>
                <Input
                  id="p-quota"
                  className="w-28"
                  inputMode="decimal"
                  value={form.grant_usd}
                  onChange={(e) => setForm((f) => ({ ...f, grant_usd: e.target.value }))}
                />
              </div>
              <div className="flex flex-col gap-1.5">
                <Label>{t('admin:planPeriod')}</Label>
                <Segmented
                  className="h-9"
                  ariaLabel={t('admin:planPeriod')}
                  value={form.period}
                  onChange={(period) => setForm((f) => ({ ...f, period }))}
                  options={PERIODS.map((p) => ({ value: p, label: periodLabel(p) }))}
                />
              </div>
              <div className="flex flex-col gap-1.5">
                <Label htmlFor="p-duration">{t('admin:planDuration')}</Label>
                <Input
                  id="p-duration"
                  className="w-28"
                  inputMode="numeric"
                  value={form.duration_days}
                  onChange={(e) => setForm((f) => ({ ...f, duration_days: e.target.value }))}
                />
              </div>
            </div>
            <p className="text-xs text-muted-foreground">
              {durationDays === undefined
                ? t('admin:planDurationRequired')
                : t('admin:planSubSummary', {
                    quota: money(grantMicro),
                    period: periodLabel(form.period),
                    days: durationDays,
                  })}
            </p>
          </FieldGroup>

          <FieldGroup title={t('admin:planSale')} hint={t('admin:planSaleHint')}>
            <div className="flex flex-wrap items-end gap-3">
              <div className="flex flex-col gap-1.5">
                <Label htmlFor="p-price">{t('admin:planPrice')}</Label>
                <Input
                  id="p-price"
                  className="w-28"
                  inputMode="decimal"
                  value={form.price_usd}
                  placeholder={t('admin:planPriceNotForSale')}
                  onChange={(e) => setForm((f) => ({ ...f, price_usd: e.target.value }))}
                />
              </div>
              <div className="flex flex-col gap-1.5">
                <Label htmlFor="p-group">{t('admin:planSubGroup')}</Label>
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
                <Label htmlFor="p-sort">{t('admin:planSortOrder')}</Label>
                <Input
                  id="p-sort"
                  className="w-20"
                  inputMode="numeric"
                  value={form.sort_order}
                  onChange={(e) => setForm((f) => ({ ...f, sort_order: e.target.value }))}
                />
              </div>
            </div>
            <div className="flex flex-col gap-1.5">
              <Label htmlFor="p-desc">{t('admin:planDescription')}</Label>
              <Textarea
                id="p-desc"
                rows={2}
                value={form.description}
                placeholder={t('admin:planDescriptionHint')}
                onChange={(e) => setForm((f) => ({ ...f, description: e.target.value }))}
              />
            </div>
          </FieldGroup>
        </>
      )}
    </Drawer>
  )
}
