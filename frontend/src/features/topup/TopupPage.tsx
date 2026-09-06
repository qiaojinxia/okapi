import { useMutation, useQueryClient } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import dayjs from 'dayjs'
import { ArrowRight, CreditCard, Receipt, Ticket, Wallet } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Alert } from '@/components/ui/alert'
import { Button, buttonVariants } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Field } from '@/components/ui/field'
import { Input } from '@/components/ui/input'
import { PageHeader } from '@/components/ui/page'
import { Segmented } from '@/components/ui/segmented'
import { toast } from '@/components/ui/toast'
import { gotoPayment } from '@/features/topup/payment'
import type { TopupResp } from '@/features/topup/payment'
import { useMe } from '@/hooks/use-auth'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatMoney } from '@/lib/money'
import { qk } from '@/lib/query-keys'
import { cn } from '@/lib/utils'

/// 起充下限与后端 MIN_TOPUP_MICRO 对齐（$1），避免下单后才被 400 拒。
const MIN_TOPUP_USD = 1
const PRESETS = [5, 10, 20, 50, 100]

/// 核销响应：普通码入钱包（`balance_after_micro`）；绑订阅套餐的码激活 / 续期订阅
/// （`subscription` + `outcome`，`amount_micro` 为 0，§11.28）。
interface RedeemResp {
  amount_micro: number
  balance_after_micro?: number
  plan_code: string | null
  granted_group?: string | null
  balance_valid_days?: number | null
  subscription?: { display_name: string; remaining_micro: number; expires_at: string }
  outcome?: 'activated' | 'renewed'
}

/// 充值页：在线充值为主流程放左侧大卡，兑换码为次流程放右侧；
/// 顶部常驻当前余额——充值前后的对照数字就在眼前。
export function TopupPage() {
  const { t, i18n } = useTranslation()
  const me = useMe()
  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        title={t('portal:topupNav')}
        description={t('portal:topupDesc')}
        icon={Wallet}
        meta={
          me.data && (
            <span className="rounded-full bg-primary/10 px-2.5 py-0.5 text-xs font-medium text-primary tabular-nums">
              {t('common:balance')} {formatMoney(me.data.balance_micro, i18n.language)}
            </span>
          )
        }
        action={
          <Link to="/portal/ledger" className={buttonVariants({ variant: 'outline' })}>
            <Receipt className="h-4 w-4" />
            {t('portal:topupSeeOrders')}
          </Link>
        }
      />
      <div className="grid gap-4 lg:grid-cols-[minmax(0,3fr)_minmax(0,2fr)]">
        <RechargeCard />
        <RedeemCard />
      </div>
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
      if (data.subscription !== undefined) {
        toast.success(data.outcome === 'renewed' ? t('portal:subRenewed') : t('portal:subActivated'))
        void queryClient.invalidateQueries({ queryKey: qk.mySubscription })
      } else {
        toast.success(t('portal:redeemCredited', { amount: formatMoney(data.amount_micro, i18n.language) }))
      }
      void queryClient.invalidateQueries({ queryKey: qk.me })
    },
    onError: (err) => {
      setResult(null)
      setError(describeError(err))
    },
  })

  return (
    <Card className="self-start">
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <Ticket className="h-4 w-4 text-primary" />
          {t('portal:redeemTitle')}
        </CardTitle>
        <CardDescription>{t('portal:redeemHint')}</CardDescription>
      </CardHeader>
      <CardContent className="flex flex-col gap-4 pt-3">
        <form
          className="flex flex-col gap-3"
          onSubmit={(e) => {
            e.preventDefault()
            if (code.trim() !== '') redeem.mutate()
          }}
        >
          <Field label={t('portal:redeemCode')} htmlFor="code" error={error}>
            <Input
              id="code"
              className="font-mono"
              autoComplete="off"
              value={code}
              placeholder={t('portal:redeemPlaceholder')}
              onChange={(e) => {
                setCode(e.target.value)
                setError(null)
              }}
            />
          </Field>
          <Button type="submit" variant="secondary" loading={redeem.isPending} disabled={code.trim().length === 0}>
            {t('portal:redeemSubmit')}
          </Button>
        </form>

        {result !== null && result.subscription !== undefined && (
          <Alert
            tone="success"
            title={result.outcome === 'renewed' ? t('portal:subRenewed') : t('portal:subActivated')}
            onClose={() => setResult(null)}
          >
            <dl className="mt-1 grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-xs">
              <dt>{t('portal:redeemPlan')}</dt>
              <dd className="text-foreground">
                {result.subscription.display_name}{' '}
                <span className="font-mono text-muted-foreground">{result.plan_code}</span>
              </dd>
              <dt>{t('portal:subRemaining')}</dt>
              <dd className="font-medium text-foreground tabular-nums">
                {formatMoney(result.subscription.remaining_micro, i18n.language)}
              </dd>
              <dt>{t('portal:subEnded')}</dt>
              <dd className="text-foreground">{dayjs(result.subscription.expires_at).format('YYYY-MM-DD HH:mm')}</dd>
            </dl>
            <Link to="/portal/plans" className="mt-2 inline-block text-xs text-primary underline decoration-dotted">
              {t('portal:plansNav')} →
            </Link>
          </Alert>
        )}
        {result !== null && result.subscription === undefined && (
          <Alert tone="success" title={t('portal:redeemCredited', { amount: formatMoney(result.amount_micro, i18n.language) })} onClose={() => setResult(null)}>
            <dl className="mt-1 grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-xs">
              <dt>{t('common:balance')}</dt>
              <dd className="font-medium text-foreground tabular-nums">
                {formatMoney(result.balance_after_micro ?? 0, i18n.language)}
              </dd>
              {result.plan_code !== null && (
                <>
                  <dt>{t('portal:redeemPlan')}</dt>
                  <dd className="font-mono text-foreground">{result.plan_code}</dd>
                </>
              )}
              {result.granted_group != null && (
                <>
                  <dt>{t('portal:redeemGroup')}</dt>
                  <dd className="font-mono text-foreground">{result.granted_group}</dd>
                </>
              )}
              {result.balance_valid_days != null && (
                <>
                  <dt>{t('portal:redeemValidDaysLabel')}</dt>
                  <dd className="text-foreground">
                    {t('portal:redeemValidDays', { days: result.balance_valid_days })}
                  </dd>
                </>
              )}
            </dl>
          </Alert>
        )}
      </CardContent>
    </Card>
  )
}

function RechargeCard() {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const [usd, setUsd] = useState('10')
  const [gateway, setGateway] = useState('epay')
  const [error, setError] = useState<string | null>(null)

  const amountMicro = Math.round((Number(usd) || 0) * 1_000_000)
  const belowMin = amountMicro < MIN_TOPUP_USD * 1_000_000
  const presetHit = PRESETS.find((v) => String(v) === usd)

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
        <CardTitle className="flex items-center gap-2">
          <CreditCard className="h-4 w-4 text-primary" />
          {t('portal:topupTitle')}
        </CardTitle>
        <CardDescription>{t('portal:topupHint', { min: MIN_TOPUP_USD })}</CardDescription>
      </CardHeader>
      <CardContent className="flex flex-col gap-5 pt-3">
        <form
          className="flex flex-col gap-5"
          onSubmit={(e) => {
            e.preventDefault()
            if (!belowMin) topup.mutate()
          }}
        >
          {/* 快捷档（new-api / Sub2API 充值页同有）：多数人充的就是这几个整数，少敲一次键盘 */}
          <div className="flex flex-col gap-2">
            <span className="text-xs font-medium text-muted-foreground">{t('portal:topupQuick')}</span>
            <div className="grid grid-cols-5 gap-2">
              {PRESETS.map((v) => {
                const on = presetHit === v
                return (
                  <button
                    key={v}
                    type="button"
                    aria-pressed={on}
                    onClick={() => setUsd(String(v))}
                    className={cn(
                      'flex h-12 items-center justify-center rounded-lg border text-sm font-semibold tabular-nums transition-colors outline-none',
                      'focus-visible:ring-2 focus-visible:ring-primary/40',
                      on
                        ? 'border-primary bg-primary/8 text-primary'
                        : 'border-border bg-card text-foreground hover:border-muted-foreground/40 hover:bg-accent/40',
                    )}
                  >
                    ${v}
                  </button>
                )
              })}
            </div>
          </div>

          <div className="grid gap-4 sm:grid-cols-2">
            <Field
              label={t('portal:topupAmount')}
              htmlFor="usd"
              error={belowMin ? t('portal:topupBelowMin', { min: MIN_TOPUP_USD }) : null}
            >
              <div className="relative">
                <span className="pointer-events-none absolute top-1/2 left-3 -translate-y-1/2 text-sm text-muted-foreground">
                  $
                </span>
                <Input
                  id="usd"
                  className="pl-7 text-base font-semibold tabular-nums"
                  inputMode="decimal"
                  value={usd}
                  onChange={(e) => setUsd(e.target.value)}
                />
              </div>
            </Field>
            <Field label={t('portal:topupGateway')}>
              <Segmented
                className="h-9 w-full [&>button]:flex-1"
                ariaLabel={t('portal:topupGateway')}
                value={gateway}
                onChange={setGateway}
                options={[
                  { value: 'epay', label: 'epay' },
                  { value: 'stripe', label: 'Stripe' },
                ]}
              />
            </Field>
          </div>

          {error !== null && <Alert tone="destructive">{error}</Alert>}

          <div className="flex flex-wrap items-center justify-between gap-3 rounded-lg bg-muted/50 px-4 py-3">
            <div className="flex flex-col">
              <span className="text-xs text-muted-foreground">{t('portal:topupWillCredit')}</span>
              <span className="text-lg font-semibold tabular-nums">
                {formatMoney(belowMin ? 0 : amountMicro, locale)}
              </span>
            </div>
            <Button type="submit" size="lg" loading={topup.isPending} disabled={belowMin}>
              {t('portal:topupSubmit')}
              <ArrowRight className="h-4 w-4" />
            </Button>
          </div>
        </form>
      </CardContent>
    </Card>
  )
}
