import { useMutation, useQuery } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { CreateResp } from '@/features/codes/types'
import { Button } from '@/components/ui/button'
import { Drawer, FieldGroup } from '@/components/ui/drawer'
import { Input, Label } from '@/components/ui/input'
import { Select } from '@/components/ui/select'
import { Textarea } from '@/components/ui/textarea'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatMoney } from '@/lib/money'

/// 生成批次抽屉。生成后码明文只此一次可见，故结果留在抽屉里直到用户主动关闭。
export function GenerateDrawer({ onClose, onDone }: { onClose: () => void; onDone: () => void }) {
  const { t, i18n } = useTranslation()
  const [form, setForm] = useState({
    count: '10',
    amount_usd: '10',
    plan_code: '',
    bind_user_id: '',
    max_per_ip: '',
    expires_at: '',
  })
  const [error, setError] = useState<string | null>(null)
  const [result, setResult] = useState<CreateResp | null>(null)
  const [copied, setCopied] = useState(false)

  const plans = useQuery({
    queryKey: ['admin', 'plans'],
    queryFn: () => apiFetch<{ data: { plan_code: string }[] }>('/admin/plans'),
  })

  const amountMicro = Math.round((Number(form.amount_usd) || 0) * 1_000_000)

  const create = useMutation({
    mutationFn: () =>
      apiFetch<CreateResp>('/admin/redemptions', {
        method: 'POST',
        body: {
          count: Number(form.count) || 0,
          amount_micro: amountMicro,
          plan_code: form.plan_code === '' ? undefined : form.plan_code,
          bind_user_id:
            form.bind_user_id.trim() === '' ? undefined : Number(form.bind_user_id) || undefined,
          max_per_ip:
            form.max_per_ip.trim() === '' ? undefined : Number(form.max_per_ip) || undefined,
          // datetime-local 无时区，按本地时间转 UTC ISO 交给后端
          expires_at:
            form.expires_at === '' ? undefined : new Date(form.expires_at).toISOString(),
        },
      }),
    onSuccess: (data) => {
      setError(null)
      setCopied(false)
      setResult(data)
      onDone()
    },
    onError: (err) => {
      setResult(null)
      setError(describeError(err))
    },
  })

  const copyAll = () => {
    if (result === null) return
    void navigator.clipboard.writeText(result.codes.join('\n')).then(() => setCopied(true))
  }

  return (
    <Drawer
      open
      onClose={onClose}
      title={t('admin:redeemTitle')}
      description={t('admin:redeemHint')}
      footer={
        <>
          {error !== null && <span className="mr-auto text-xs text-destructive">{error}</span>}
          <Button variant="ghost" onClick={onClose}>
            {result === null ? t('common:cancel') : t('common:close')}
          </Button>
          {result === null && (
            <Button
              disabled={create.isPending || amountMicro <= 0}
              onClick={() => create.mutate()}
            >
              {t('admin:redeemGenerate')}
            </Button>
          )}
        </>
      }
    >
      {result === null ? (
        <>
          <FieldGroup title={t('common:basicInfo')} hint={t('admin:redeemBasicHint')}>
            <div className="grid grid-cols-2 gap-3">
              <div className="flex flex-col gap-1.5">
                <Label htmlFor="c-count">{t('admin:redeemCount')}</Label>
                <Input
                  id="c-count"
                  inputMode="numeric"
                  value={form.count}
                  onChange={(e) => setForm((f) => ({ ...f, count: e.target.value }))}
                />
              </div>
              <div className="flex flex-col gap-1.5">
                <Label htmlFor="c-amount">{t('admin:redeemAmount')}</Label>
                <Input
                  id="c-amount"
                  inputMode="decimal"
                  value={form.amount_usd}
                  onChange={(e) => setForm((f) => ({ ...f, amount_usd: e.target.value }))}
                />
                <span className="text-xs text-muted-foreground">
                  {t('admin:redeemFaceValue', {
                    amount: formatMoney(amountMicro, i18n.language),
                  })}
                </span>
              </div>
              <div className="col-span-2 flex flex-col gap-1.5">
                <Label htmlFor="c-plan">{t('admin:redeemPlanCode')}</Label>
                <Select
                  id="c-plan"
                  value={form.plan_code}
                  onChange={(v) => setForm((f) => ({ ...f, plan_code: v }))}
                  placeholder={t('admin:redeemNoPlan')}
                  options={(plans.data?.data ?? []).map((p) => ({
                    value: p.plan_code,
                    label: p.plan_code,
                  }))}
                />
                <span className="text-xs text-muted-foreground">
                  {t('admin:redeemPlanHint')}
                </span>
              </div>
            </div>
          </FieldGroup>

          <FieldGroup title={t('admin:redeemLimits')} hint={t('admin:redeemLimitsHint')}>
            <div className="grid grid-cols-2 gap-3">
              <div className="flex flex-col gap-1.5">
                <Label htmlFor="c-bind">{t('admin:redeemBindUser')}</Label>
                <Input
                  id="c-bind"
                  inputMode="numeric"
                  value={form.bind_user_id}
                  placeholder={t('admin:redeemAnyone')}
                  onChange={(e) => setForm((f) => ({ ...f, bind_user_id: e.target.value }))}
                />
              </div>
              <div className="flex flex-col gap-1.5">
                <Label htmlFor="c-ip">{t('admin:redeemMaxPerIp')}</Label>
                <Input
                  id="c-ip"
                  inputMode="numeric"
                  value={form.max_per_ip}
                  placeholder={t('team:noLimit')}
                  onChange={(e) => setForm((f) => ({ ...f, max_per_ip: e.target.value }))}
                />
              </div>
              <div className="col-span-2 flex flex-col gap-1.5">
                <Label htmlFor="c-exp">{t('admin:redeemExpiresAt')}</Label>
                <Input
                  id="c-exp"
                  type="datetime-local"
                  className="w-60"
                  value={form.expires_at}
                  onChange={(e) => setForm((f) => ({ ...f, expires_at: e.target.value }))}
                />
              </div>
            </div>
          </FieldGroup>
        </>
      ) : (
        <FieldGroup
          title={t('admin:redeemBatch', { id: result.batch_id })}
          hint={t('admin:redeemOnceOnly')}
        >
          <Button size="sm" variant="outline" className="self-start" onClick={copyAll}>
            {copied ? t('portal:affCopied') : t('admin:redeemCopyAll')}
          </Button>
          <Textarea
            readOnly
            rows={14}
            className="font-mono text-xs"
            value={result.codes.join('\n')}
          />
        </FieldGroup>
      )}
    </Drawer>
  )
}
