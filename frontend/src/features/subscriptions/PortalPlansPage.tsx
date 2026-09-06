import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import dayjs from 'dayjs'
import { ArrowRight, CalendarClock, Check, Package, RefreshCw, Ticket } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Alert } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Field } from '@/components/ui/field'
import { PageHeader } from '@/components/ui/page'
import { Segmented } from '@/components/ui/segmented'
import { TableSkeleton } from '@/components/ui/skeleton'
import { EmptyState, ErrorState } from '@/components/ui/state'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { gotoPayment } from '@/features/topup/payment'
import type { TopupResp } from '@/features/topup/payment'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatMoney } from '@/lib/money'
import { qk } from '@/lib/query-keys'
import { cn } from '@/lib/utils'

export interface PublicPlan {
  plan_code: string
  display_name: string
  quota_micro: number
  price_micro: number
  purchasable: boolean
  period: 1 | 2 | 3
  duration_days: number
  group_code: string | null
  description: string | null
  sort_order: number
}

export interface SubscriptionView {
  id: number
  plan_code: string
  display_name: string
  period: 1 | 2 | 3
  status: 1 | 2 | 3
  quota_micro: number
  remaining_micro: number
  group_code: string | null
  granted_group: boolean
  starts_at: string
  expires_at: string
  window_start: string
  window_end: string
  /// 0 = 池当前不可用（滚窗前的短暂间隙）。
  pool_until_unix: number
  source: string
}

/// 历史行（后端 store 结构直出，无 remaining）。
interface SubscriptionHistory {
  id: number
  plan_code: string
  display_name: string
  status: 1 | 2 | 3
  quota_micro: number
  starts_at: string
  expires_at: string
}

export interface MySubscription {
  subscription: SubscriptionView | null
  history: SubscriptionHistory[]
}

/// 门户套餐页（IMPLEMENTATION §11.28）：上方"我的订阅"，下方在售套餐卡。
///
/// 一个用户同一时刻只有一个订阅：当前套餐卡显示"续期"，其它卡在激活期内禁用并说明
/// 原因——后端 409 也拦，但付款前就该让人知道，而不是跳到支付页再被拒。
export function PortalPlansPage() {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const queryClient = useQueryClient()
  const [gateway, setGateway] = useState('epay')
  const [error, setError] = useState<string | null>(null)

  const plans = useQuery({
    queryKey: qk.publicPlans,
    queryFn: () => apiFetch<{ data: PublicPlan[] }>('/api/plans'),
  })
  const mine = useQuery({
    queryKey: qk.mySubscription,
    queryFn: () => apiFetch<MySubscription>('/api/me/subscription'),
    // 滚窗由 worker 每分钟推进；页面停留时跟着刷
    refetchInterval: 60_000,
  })
  const current = mine.data?.subscription ?? null

  const checkout = useMutation({
    mutationFn: (plan_code: string) =>
      apiFetch<TopupResp>('/api/me/subscriptions/checkout', {
        method: 'POST',
        body: { plan_code, gateway },
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
    <div className="flex flex-col gap-4">
      <PageHeader
        title={t('portal:plansNav')}
        description={t('portal:plansDesc')}
        icon={Package}
        action={
          <Field label={t('portal:planPayWith')}>
            <Segmented
              className="h-9"
              ariaLabel={t('portal:planPayWith')}
              value={gateway}
              onChange={setGateway}
              options={[
                { value: 'epay', label: 'epay' },
                { value: 'stripe', label: 'Stripe' },
              ]}
            />
          </Field>
        }
      />

      <CurrentSubscription
        data={mine.data ?? null}
        pending={mine.isPending}
        error={mine.isError ? describeError(mine.error) : null}
        onRefresh={() => void queryClient.invalidateQueries({ queryKey: qk.mySubscription })}
      />

      {error !== null && <Alert tone="destructive" onClose={() => setError(null)}>{error}</Alert>}

      {plans.isError ? (
        <ErrorState message={describeError(plans.error)} onRetry={() => void plans.refetch()} />
      ) : plans.isPending ? (
        <TableSkeleton rows={3} cols={3} />
      ) : plans.data.data.length === 0 ? (
        <EmptyState hint={t('portal:plansEmpty')} />
      ) : (
        <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-3">
          {plans.data.data.map((p) => (
            <PlanCard
              key={p.plan_code}
              plan={p}
              current={current}
              busy={checkout.isPending && checkout.variables === p.plan_code}
              onBuy={() => checkout.mutate(p.plan_code)}
              locale={locale}
            />
          ))}
        </div>
      )}
    </div>
  )
}

function CurrentSubscription({
  data,
  pending,
  error,
  onRefresh,
}: {
  data: MySubscription | null
  pending: boolean
  error: string | null
  onRefresh: () => void
}) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  if (error !== null) return <ErrorState message={error} />
  if (pending || data === null) return <TableSkeleton rows={2} cols={3} />
  const sub = data.subscription
  const fmt = (iso: string) => dayjs(iso).format('YYYY-MM-DD HH:mm')

  return (
    <Card>
      <CardHeader className="flex flex-row items-start justify-between gap-3">
        <div className="flex flex-col gap-1">
          <CardTitle className="flex items-center gap-2">
            <CalendarClock className="h-4 w-4 text-primary" />
            {t('portal:subCurrentTitle')}
          </CardTitle>
          {sub === null && <CardDescription>{t('portal:subNone')}</CardDescription>}
        </div>
        <Button variant="ghost" size="sm" onClick={onRefresh} aria-label={t('common:refresh')}>
          <RefreshCw className="h-4 w-4" />
        </Button>
      </CardHeader>
      {sub !== null && (
        <CardContent className="flex flex-col gap-4 pt-0">
          <div className="flex flex-wrap items-center gap-2">
            <span className="text-base font-semibold">{sub.display_name}</span>
            <Badge variant="muted" className="font-mono">{sub.plan_code}</Badge>
            <Badge variant="success">{t('portal:subStatus_1')}</Badge>
            {sub.group_code !== null && (
              <Badge variant="info">{t('portal:planWithGroup', { group: sub.group_code })}</Badge>
            )}
          </div>
          <QuotaBar remaining={sub.remaining_micro} quota={sub.quota_micro} locale={locale} />
          <div className="flex flex-wrap gap-x-5 gap-y-1 text-xs text-muted-foreground">
            <span>{t('portal:subResetsAt', { at: fmt(sub.window_end) })}</span>
            <span>{t('portal:subExpiresAt', { at: fmt(sub.expires_at) })}</span>
            {sub.pool_until_unix === 0 && <span className="text-warning">{t('portal:subPoolGap')}</span>}
          </div>
        </CardContent>
      )}
      {data.history.some((h) => h.status !== 1) && (
        <CardContent className="pt-0">
          <details>
            <summary className="cursor-pointer text-xs text-muted-foreground">{t('portal:subHistory')}</summary>
            <Table className="mt-2">
              <THead>
                <Tr>
                  <Th>{t('admin:planName')}</Th>
                  <Th numeric>{t('admin:planQuota')}</Th>
                  <Th>{t('common:status')}</Th>
                  <Th>{t('portal:subEnded')}</Th>
                </Tr>
              </THead>
              <TBody>
                {data.history
                  .filter((h) => h.status !== 1)
                  .map((h) => (
                    <Tr key={h.id}>
                      <Td>
                        {h.display_name}{' '}
                        <span className="font-mono text-xs text-muted-foreground">{h.plan_code}</span>
                      </Td>
                      <Td numeric>{formatMoney(h.quota_micro, locale)}</Td>
                      <Td>
                        <Badge variant="muted">{t(`portal:subStatus_${h.status}`)}</Badge>
                      </Td>
                      <Td className="text-xs">{fmt(h.expires_at)}</Td>
                    </Tr>
                  ))}
              </TBody>
            </Table>
          </details>
        </CardContent>
      )}
    </Card>
  )
}

/// 本窗剩余进度条：越界为负已由后端钳 0。
function QuotaBar({ remaining, quota, locale }: { remaining: number; quota: number; locale: string }) {
  const { t } = useTranslation()
  const pct = quota > 0 ? Math.min(100, Math.max(0, (remaining / quota) * 100)) : 0
  const tone = pct <= 10 ? 'bg-destructive' : pct <= 30 ? 'bg-warning' : 'bg-primary'
  return (
    <div className="flex flex-col gap-1.5">
      <div className="flex items-baseline justify-between">
        <span className="text-xs text-muted-foreground">{t('portal:subRemaining')}</span>
        <span className="text-sm font-semibold tabular-nums">
          {formatMoney(remaining, locale)}
          <span className="text-xs font-normal text-muted-foreground"> / {formatMoney(quota, locale)}</span>
        </span>
      </div>
      <div className="h-2 w-full overflow-hidden rounded-full bg-muted" role="progressbar" aria-valuenow={Math.round(pct)} aria-valuemin={0} aria-valuemax={100}>
        <div className={cn('h-full rounded-full transition-[width]', tone)} style={{ width: `${pct}%` }} />
      </div>
    </div>
  )
}

function PlanCard({
  plan,
  current,
  busy,
  onBuy,
  locale,
}: {
  plan: PublicPlan
  current: SubscriptionView | null
  busy: boolean
  onBuy: () => void
  locale: string
}) {
  const { t } = useTranslation()
  const isCurrent = current?.plan_code === plan.plan_code
  const blocked = current !== null && !isCurrent
  return (
    <Card className={cn('flex flex-col', isCurrent && 'border-primary/50 ring-1 ring-primary/20')}>
      <CardHeader>
        <div className="flex items-start justify-between gap-2">
          <CardTitle>{plan.display_name}</CardTitle>
          {isCurrent && (
            <Badge variant="default">
              <Check className="mr-1 h-3 w-3" />
              {t('portal:planCurrent')}
            </Badge>
          )}
        </div>
        <CardDescription className="font-mono text-xs">{plan.plan_code}</CardDescription>
      </CardHeader>
      <CardContent className="flex flex-1 flex-col gap-4 pt-0">
        <div className="flex flex-col">
          <span className="text-2xl font-semibold tabular-nums">
            {t(`portal:planPerPeriod_${plan.period}`, { amount: formatMoney(plan.quota_micro, locale) })}
          </span>
          <span className="text-xs text-muted-foreground">{t('portal:planValidFor', { days: plan.duration_days })}</span>
        </div>
        {plan.group_code !== null && (
          <Badge variant="info" className="self-start">
            {t('portal:planWithGroup', { group: plan.group_code })}
          </Badge>
        )}
        {plan.description !== null && plan.description !== '' && (
          <p className="text-sm text-muted-foreground">{plan.description}</p>
        )}
        <div className="mt-auto flex items-center justify-between gap-3 border-t border-border pt-4">
          <span className="text-lg font-semibold tabular-nums">
            {plan.purchasable ? formatMoney(plan.price_micro, locale) : (
              <span className="inline-flex items-center gap-1 text-sm font-normal text-muted-foreground">
                <Ticket className="h-4 w-4" />
                {t('portal:planNotForSale')}
              </span>
            )}
          </span>
          {plan.purchasable && (
            <Button
              size="sm"
              variant={isCurrent ? 'outline' : 'default'}
              loading={busy}
              disabled={blocked}
              title={blocked ? t('portal:planOtherActive') : undefined}
              onClick={onBuy}
            >
              {isCurrent ? t('portal:planRenew') : t('portal:planBuy')}
              <ArrowRight className="h-4 w-4" />
            </Button>
          )}
        </div>
        {blocked && plan.purchasable && (
          <p className="text-xs text-muted-foreground">{t('portal:planOtherActive')}</p>
        )}
      </CardContent>
    </Card>
  )
}
