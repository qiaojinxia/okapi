import { useMutation } from '@tanstack/react-query'
import { createFileRoute } from '@tanstack/react-router'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Input, Label } from '@/components/ui/input'
import { Textarea } from '@/components/ui/textarea'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatMoney } from '@/lib/money'

// 路径与后端 API 的 /admin/redemptions 区分开：同名可行（console 已按 Accept 做
// 导航协商），但页面是管理视图、API 是"建批次"动作，分开更少歧义。
export const Route = createFileRoute('/admin/codes')({
  component: RedemptionsPage,
})

interface CreateResp {
  batch_id: string
  codes: string[]
}

function RedemptionsPage() {
  return (
    <div className="flex flex-col gap-4">
      <GenerateCard />
      <PlanCard />
    </div>
  )
}

function GenerateCard() {
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

  const amountMicro = Math.round((Number(form.amount_usd) || 0) * 1_000_000)

  const create = useMutation({
    mutationFn: () =>
      apiFetch<CreateResp>('/admin/redemptions', {
        method: 'POST',
        body: {
          count: Number(form.count) || 0,
          amount_micro: amountMicro,
          plan_code: form.plan_code.trim() === '' ? undefined : form.plan_code.trim(),
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

  const fields: ReadonlyArray<readonly [keyof typeof form, string]> = [
    ['count', t('admin:redeemCount')],
    ['amount_usd', t('admin:redeemAmount')],
    ['plan_code', t('admin:redeemPlanCode')],
    ['bind_user_id', t('admin:redeemBindUser')],
    ['max_per_ip', t('admin:redeemMaxPerIp')],
  ]

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('admin:redeemTitle')}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        <p className="text-xs text-muted-foreground">{t('admin:redeemHint')}</p>
        <div className="grid grid-cols-2 gap-3 sm:grid-cols-3">
          {fields.map(([field, label]) => (
            <div key={field} className="flex flex-col gap-1.5">
              <Label htmlFor={field}>{label}</Label>
              <Input
                id={field}
                value={form[field]}
                onChange={(e) => setForm((f) => ({ ...f, [field]: e.target.value }))}
              />
            </div>
          ))}
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="expires_at">{t('admin:redeemExpiresAt')}</Label>
            <Input
              id="expires_at"
              type="datetime-local"
              value={form.expires_at}
              onChange={(e) => setForm((f) => ({ ...f, expires_at: e.target.value }))}
            />
          </div>
        </div>
        <div className="flex items-center gap-3">
          <Button disabled={create.isPending || amountMicro <= 0} onClick={() => create.mutate()}>
            {t('admin:redeemGenerate')}
          </Button>
          <span className="text-xs text-muted-foreground">
            {t('admin:redeemFaceValue', { amount: formatMoney(amountMicro, i18n.language) })}
          </span>
          {error !== null && <span className="text-xs text-destructive">{error}</span>}
        </div>

        {result !== null && (
          <div className="flex flex-col gap-2 border-t border-border pt-3">
            <div className="flex items-center gap-3">
              <Label>{t('admin:redeemBatch', { id: result.batch_id })}</Label>
              <Button size="sm" variant="outline" onClick={copyAll}>
                {copied ? t('portal:affCopied') : t('admin:redeemCopyAll')}
              </Button>
            </div>
            <p className="text-xs text-destructive">{t('admin:redeemOnceOnly')}</p>
            <Textarea readOnly rows={8} className="font-mono text-xs" value={result.codes.join('\n')} />
          </div>
        )}
      </CardContent>
    </Card>
  )
}

function PlanCard() {
  const { t } = useTranslation()
  const [form, setForm] = useState({
    plan_code: '',
    display_name: '',
    grant_usd: '10',
    group_code: '',
    balance_valid_days: '',
  })
  const [msg, setMsg] = useState<string | null>(null)

  const grantMicro = Math.round((Number(form.grant_usd) || 0) * 1_000_000)

  const upsert = useMutation({
    mutationFn: () =>
      apiFetch<{ plan_id: number }>('/admin/plans', {
        method: 'POST',
        body: {
          plan_code: form.plan_code.trim(),
          display_name: form.display_name.trim(),
          grant_micro: grantMicro,
          group_code: form.group_code.trim() === '' ? undefined : form.group_code.trim(),
          balance_valid_days:
            form.balance_valid_days.trim() === ''
              ? undefined
              : Number(form.balance_valid_days) || undefined,
        },
      }),
    onSuccess: () => setMsg(t('common:success')),
    onError: (err) => setMsg(describeError(err)),
  })

  const fields: ReadonlyArray<readonly [keyof typeof form, string]> = [
    ['plan_code', t('admin:planCode')],
    ['display_name', t('admin:planName')],
    ['grant_usd', t('admin:planGrant')],
    ['group_code', t('admin:planGroup')],
    ['balance_valid_days', t('admin:planValidDays')],
  ]

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('admin:planTitle')}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        <p className="text-xs text-muted-foreground">{t('admin:planHint')}</p>
        <div className="grid grid-cols-2 gap-3 sm:grid-cols-3">
          {fields.map(([field, label]) => (
            <div key={field} className="flex flex-col gap-1.5">
              <Label htmlFor={field}>{label}</Label>
              <Input
                id={field}
                value={form[field]}
                onChange={(e) => setForm((f) => ({ ...f, [field]: e.target.value }))}
              />
            </div>
          ))}
        </div>
        <div className="flex items-center gap-3">
          <Button
            disabled={upsert.isPending || form.plan_code.trim() === '' || grantMicro <= 0}
            onClick={() => upsert.mutate()}
          >
            {t('admin:planUpsert')}
          </Button>
          {msg !== null && <span className="text-xs text-muted-foreground">{msg}</span>}
        </div>
      </CardContent>
    </Card>
  )
}
