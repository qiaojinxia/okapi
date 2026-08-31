import { Bar, BarChart, CartesianGrid, ResponsiveContainer, Tooltip, XAxis, YAxis } from 'recharts'
import { useQuery } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import type { MarginResp } from '@/features/stats/types'
import { Badge } from '@/components/ui/badge'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatBp, formatCount, formatMoney } from '@/lib/money'
import { healthVariant } from '@/features/stats/ChannelHealthCard'
import { qk } from '@/lib/query-keys'

export function RevenueCard({ days }: { days: number }) {
  const { t, i18n } = useTranslation()
  const q = useQuery({
    queryKey: qk.statsMargin(days),
    queryFn: () => apiFetch<MarginResp>(`/admin/stats/margin?days=${days}`),
  })
  const total = q.data?.total
  const chart = (q.data?.data ?? []).map((d) => ({
    day: d.day.slice(5),
    amount: d.amount_micro / 1_000_000,
    discount: d.discount_micro / 1_000_000,
  }))

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('admin:statRevenue')}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        <p className="text-xs text-muted-foreground">{t('admin:statRevenueHint')}</p>
        {q.isError ? (
          <p className="text-sm text-destructive">{describeError(q.error)}</p>
        ) : (
          <>
            <div className="flex flex-wrap gap-2">
              <Badge variant="muted">
                {t('admin:statAmount')} {formatMoney(total?.amount_micro ?? 0, i18n.language)}
              </Badge>
              <Badge variant="muted">
                {t('admin:statDiscount')} {formatMoney(total?.discount_micro ?? 0, i18n.language)}
              </Badge>
              <Badge variant="muted">
                {t('common:requests')} {formatCount(total?.requests ?? 0, i18n.language)}
              </Badge>
              <Badge variant={healthVariant(total?.error_rate_bp ?? 0)}>
                {t('admin:statErrorRate')} {formatBp(total?.error_rate_bp ?? 0, i18n.language)}
              </Badge>
              {(total?.upstream_cost_micro ?? 0) > 0 ? (
                <Badge variant="success">
                  {t('admin:statMargin')} {formatMoney(total?.margin_micro ?? 0, i18n.language)}
                </Badge>
              ) : (
                <Badge variant="muted">{t('admin:statMarginPending')}</Badge>
              )}
            </div>
            <div className="h-64">
              <ResponsiveContainer width="100%" height="100%">
                <BarChart data={chart}>
                  <CartesianGrid strokeDasharray="3 3" className="stroke-border" />
                  <XAxis dataKey="day" fontSize={12} />
                  <YAxis fontSize={12} />
                  <Tooltip />
                  <Bar dataKey="amount" name={t('admin:statAmount')} fill="var(--color-primary)" />
                  <Bar
                    dataKey="discount"
                    name={t('admin:statDiscount')}
                    fill="var(--color-muted-foreground)"
                  />
                </BarChart>
              </ResponsiveContainer>
            </div>
          </>
        )}
      </CardContent>
    </Card>
  )
}
