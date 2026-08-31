import { useQuery } from '@tanstack/react-query'
import { createFileRoute } from '@tanstack/react-router'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Bar, BarChart, CartesianGrid, ResponsiveContainer, Tooltip, XAxis, YAxis } from 'recharts'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { useMe } from '@/hooks/use-auth'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatMoney } from '@/lib/money'
import { qk } from '@/lib/query-keys'

export const Route = createFileRoute('/portal/')({
  component: Dashboard,
})

interface UsageRow {
  day: string
  requests: number | string
  tokens: number | string
  amount_micro: number | string
}

interface UsageResp {
  scope: string
  days: number
  total_amount_micro: number
  data: UsageRow[]
}

function Dashboard() {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const me = useMe()
  const [scope, setScope] = useState<'key' | 'user'>('key')
  const [days, setDays] = useState(7)

  const usage = useQuery({
    queryKey: qk.usage(scope, days),
    queryFn: () => apiFetch<UsageResp>(`/api/me/usage?scope=${scope}&days=${days}`),
  })

  const chartData =
    usage.data?.data.map((r) => ({
      day: r.day,
      amount: Number(r.amount_micro) / 1_000_000,
      requests: Number(r.requests),
    })) ?? []

  return (
    <div className="flex flex-col gap-4">
      <div className="grid grid-cols-1 gap-4 sm:grid-cols-3">
        <Card>
          <CardHeader>
            <CardTitle>{t('common:balance')}</CardTitle>
          </CardHeader>
          <CardContent className="text-2xl font-bold">
            {me.data ? formatMoney(me.data.balance_micro, locale) : t('common:loading')}
          </CardContent>
        </Card>
        <Card>
          <CardHeader>
            <CardTitle>{t('portal:totalSpend')}</CardTitle>
          </CardHeader>
          <CardContent className="text-2xl font-bold">
            {usage.data ? formatMoney(usage.data.total_amount_micro, locale) : '—'}
          </CardContent>
        </Card>
        <Card>
          <CardHeader>
            <CardTitle>{t('portal:group')}</CardTitle>
          </CardHeader>
          <CardContent className="text-2xl font-bold">{me.data?.group ?? '—'}</CardContent>
        </Card>
      </div>

      <Card>
        <CardHeader className="flex-row items-center justify-between">
          <CardTitle>{t('portal:usageTitle')}</CardTitle>
          <div className="flex gap-1">
            <Button
              size="sm"
              variant={scope === 'key' ? 'default' : 'outline'}
              onClick={() => setScope('key')}
            >
              {t('portal:scopeKey')}
            </Button>
            <Button
              size="sm"
              variant={scope === 'user' ? 'default' : 'outline'}
              onClick={() => setScope('user')}
            >
              {t('portal:scopeUser')}
            </Button>
            {[7, 30, 90].map((d) => (
              <Button
                key={d}
                size="sm"
                variant={days === d ? 'default' : 'outline'}
                onClick={() => setDays(d)}
              >
                {t(`common:days_${d}`)}
              </Button>
            ))}
          </div>
        </CardHeader>
        <CardContent className="h-72">
          {usage.isError ? (
            <p className="text-sm text-destructive">{describeError(usage.error)}</p>
          ) : chartData.length === 0 ? (
            <p className="text-sm text-muted-foreground">{t('common:empty')}</p>
          ) : (
            <ResponsiveContainer width="100%" height="100%">
              <BarChart data={chartData}>
                <CartesianGrid strokeDasharray="3 3" stroke="var(--border)" />
                <XAxis dataKey="day" tick={{ fontSize: 11 }} />
                <YAxis tick={{ fontSize: 11 }} />
                <Tooltip
                  formatter={(value) => formatMoney(Number(value) * 1_000_000, locale)}
                />
                <Bar dataKey="amount" fill="var(--primary)" radius={[3, 3, 0, 0]} />
              </BarChart>
            </ResponsiveContainer>
          )}
        </CardContent>
      </Card>

      <ModelBreakdownCard days={days} />
    </div>
  )
}

interface DailyRow {
  day: string
  model: string
  requests: number
  tokens: number
  amount_micro: number
  discount_micro: number
}

/// 我的消费按模型细分（/api/me/stats/daily 走 CH mv_user_model_day）。
/// 与上方按日曲线互补：曲线回答"何时花的"，这里回答"花在哪个模型上"。
function ModelBreakdownCard({ days }: { days: number }) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const daily = useQuery({
    queryKey: qk.myDaily(days),
    queryFn: () => apiFetch<{ days: number; data: DailyRow[] }>(`/api/me/stats/daily?days=${days}`),
    // CH 未启用时后端 501，属预期，不重试
    retry: false,
  })

  // 后端按 (day, model) 返回，这里合并到模型维度出占比
  const byModel = new Map<string, { amount: number; requests: number; tokens: number }>()
  for (const r of daily.data?.data ?? []) {
    const cur = byModel.get(r.model) ?? { amount: 0, requests: 0, tokens: 0 }
    cur.amount += Number(r.amount_micro)
    cur.requests += Number(r.requests)
    cur.tokens += Number(r.tokens)
    byModel.set(r.model, cur)
  }
  const rows = [...byModel.entries()]
    .map(([model, v]) => ({ model, ...v }))
    .sort((a, b) => b.amount - a.amount)
  const total = rows.reduce((sum, r) => sum + r.amount, 0)

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('portal:byModelTitle')}</CardTitle>
      </CardHeader>
      <CardContent>
        {daily.isError ? (
          <p className="text-sm text-muted-foreground">{describeError(daily.error)}</p>
        ) : rows.length === 0 ? (
          <p className="text-sm text-muted-foreground">{t('common:empty')}</p>
        ) : (
          <div className="flex flex-col gap-2">
            {rows.map((r) => (
              <div key={r.model} className="flex items-center gap-3">
                <span className="w-48 shrink-0 truncate font-mono text-xs">{r.model}</span>
                <div className="h-2 flex-1 overflow-hidden rounded bg-muted">
                  <div
                    className="h-full bg-primary"
                    style={{ width: total > 0 ? `${(r.amount / total) * 100}%` : '0%' }}
                  />
                </div>
                <span className="w-24 shrink-0 text-right text-xs">
                  {formatMoney(r.amount, locale)}
                </span>
              </div>
            ))}
          </div>
        )}
      </CardContent>
    </Card>
  )
}
