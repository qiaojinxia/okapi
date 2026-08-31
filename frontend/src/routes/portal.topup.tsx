import { useMutation, useQueryClient } from '@tanstack/react-query'
import { createFileRoute } from '@tanstack/react-router'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Input, Label } from '@/components/ui/input'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatMoney } from '@/lib/money'
import { qk } from '@/lib/query-keys'

export const Route = createFileRoute('/portal/topup')({
  component: TopupPage,
})

/// 起充下限与后端 MIN_TOPUP_MICRO 对齐（$1），避免下单后才被 400 拒。
const MIN_TOPUP_USD = 1

interface RedeemResp {
  amount_micro: number
  balance_after_micro: number
  plan_code: string | null
  granted_group: string | null
  balance_valid_days: number | null
}

interface TopupResp {
  order_no: string
  gateway: string
  pay_url: string | null
  params?: Record<string, string>
}

/// epay 要求以表单 POST 带签名参数跳转；stripe 直接跳 checkout url。
function gotoPayment(resp: TopupResp): void {
  if (resp.pay_url === null) return
  if (resp.params === undefined) {
    window.location.href = resp.pay_url
    return
  }
  const form = document.createElement('form')
  form.method = 'POST'
  form.action = resp.pay_url
  for (const [name, value] of Object.entries(resp.params)) {
    const field = document.createElement('input')
    field.type = 'hidden'
    field.name = name
    field.value = value
    form.append(field)
  }
  document.body.append(form)
  form.submit()
}

function TopupPage() {
  return (
    <div className="flex flex-col gap-4">
      <RedeemCard />
      <RechargeCard />
    </div>
  )
}

function RedeemCard() {
  const { t, i18n } = useTranslation()
  const queryClient = useQueryClient()
  const [code, setCode] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [result, setResult] = useState<RedeemResp | null>(null)

  const redeem = useMutation({
    mutationFn: () =>
      apiFetch<RedeemResp>('/api/me/redeem', { method: 'POST', body: { code: code.trim() } }),
    onSuccess: (data) => {
      setError(null)
      setResult(data)
      setCode('')
      void queryClient.invalidateQueries({ queryKey: qk.me })
    },
    onError: (err) => {
      setResult(null)
      setError(describeError(err))
    },
  })

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('portal:redeemTitle')}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        <p className="text-xs text-muted-foreground">{t('portal:redeemHint')}</p>
        <div className="flex flex-wrap items-end gap-3">
          <div className="flex min-w-72 flex-col gap-1.5">
            <Label htmlFor="code">{t('portal:redeemCode')}</Label>
            <Input
              id="code"
              className="font-mono"
              value={code}
              placeholder={t('portal:redeemPlaceholder')}
              onChange={(e) => setCode(e.target.value)}
            />
          </div>
          <Button disabled={redeem.isPending || code.trim().length === 0} onClick={() => redeem.mutate()}>
            {t('portal:redeemSubmit')}
          </Button>
        </div>
        {error !== null && <p className="text-xs text-destructive">{error}</p>}
        {result !== null && (
          <div className="flex flex-wrap items-center gap-2">
            <Badge variant="success">
              {t('portal:redeemCredited', { amount: formatMoney(result.amount_micro, i18n.language) })}
            </Badge>
            <Badge variant="muted">
              {t('common:balance')} {formatMoney(result.balance_after_micro, i18n.language)}
            </Badge>
            {result.plan_code !== null && (
              <Badge variant="muted">
                {t('portal:redeemPlan')} {result.plan_code}
              </Badge>
            )}
            {result.granted_group !== null && (
              <Badge variant="muted">
                {t('portal:redeemGroup')} {result.granted_group}
              </Badge>
            )}
            {result.balance_valid_days !== null && (
              <Badge variant="muted">
                {t('portal:redeemValidDays', { days: result.balance_valid_days })}
              </Badge>
            )}
          </div>
        )}
      </CardContent>
    </Card>
  )
}

function RechargeCard() {
  const { t } = useTranslation()
  const [usd, setUsd] = useState('10')
  const [gateway, setGateway] = useState('epay')
  const [error, setError] = useState<string | null>(null)

  const amountMicro = Math.round((Number(usd) || 0) * 1_000_000)
  const belowMin = amountMicro < MIN_TOPUP_USD * 1_000_000

  const topup = useMutation({
    mutationFn: () =>
      apiFetch<TopupResp>('/api/me/topup', {
        method: 'POST',
        body: { amount_micro: amountMicro, gateway },
      }),
    onSuccess: (data) => {
      setError(null)
      if (data.pay_url === null) {
        setError(t('portal:topupNoUrl'))
        return
      }
      gotoPayment(data)
    },
    onError: (err) => setError(describeError(err)),
  })

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('portal:topupTitle')}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        <p className="text-xs text-muted-foreground">
          {t('portal:topupHint', { min: MIN_TOPUP_USD })}
        </p>
        <div className="flex flex-wrap items-end gap-3">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="usd">{t('portal:topupAmount')}</Label>
            <Input
              id="usd"
              className="w-40"
              inputMode="decimal"
              value={usd}
              onChange={(e) => setUsd(e.target.value)}
            />
          </div>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="gateway">{t('portal:topupGateway')}</Label>
            <select
              id="gateway"
              className="h-9 rounded-md border border-input bg-card px-2 text-sm"
              value={gateway}
              onChange={(e) => setGateway(e.target.value)}
            >
              <option value="epay">epay</option>
              <option value="stripe">stripe</option>
            </select>
          </div>
          <Button disabled={topup.isPending || belowMin} onClick={() => topup.mutate()}>
            {t('portal:topupSubmit')}
          </Button>
        </div>
        {belowMin && <p className="text-xs text-destructive">{t('portal:topupBelowMin', { min: MIN_TOPUP_USD })}</p>}
        {error !== null && <p className="text-xs text-destructive">{error}</p>}
      </CardContent>
    </Card>
  )
}
