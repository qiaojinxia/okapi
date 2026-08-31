import { useQuery } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import {
  Area,
  AreaChart,
  CartesianGrid,
  Legend,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from 'recharts'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { EmptyState, ErrorState } from '@/components/ui/state'
import type { MarginResp } from '@/features/dashboard/types'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { qk } from '@/lib/query-keys'

/// 请求量与收入的按日趋势。
///
/// 用面积图而非柱状：趋势看的是走向，面积的连续感比一根根柱子更快读出"在涨还是在跌"；
/// 收入走右轴——两者量级差几个数量级，同轴会把其中一条压成一条直线。
export function TrendCard({ days }: { days: number }) {
  const { t, i18n } = useTranslation()
  const q = useQuery({
    queryKey: qk.statsMargin(days),
    queryFn: () => apiFetch<MarginResp>(`/admin/stats/margin?days=${days}`),
  })

  const chart = (q.data?.data ?? []).map((d) => ({
    day: d.day.slice(5),
    requests: d.requests,
    revenue: d.amount_micro / 1_000_000,
  }))

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('admin:trendTitle')}</CardTitle>
      </CardHeader>
      <CardContent>
        {q.isError ? (
          <ErrorState message={describeError(q.error)} />
        ) : chart.length === 0 ? (
          <EmptyState hint={t('admin:trendEmptyHint')} />
        ) : (
          <div className="h-72">
            <ResponsiveContainer width="100%" height="100%">
              <AreaChart data={chart}>
                <defs>
                  <linearGradient id="g-req" x1="0" y1="0" x2="0" y2="1">
                    <stop offset="0%" stopColor="var(--color-primary)" stopOpacity={0.35} />
                    <stop offset="100%" stopColor="var(--color-primary)" stopOpacity={0.02} />
                  </linearGradient>
                  <linearGradient id="g-rev" x1="0" y1="0" x2="0" y2="1">
                    <stop offset="0%" stopColor="var(--color-success)" stopOpacity={0.35} />
                    <stop offset="100%" stopColor="var(--color-success)" stopOpacity={0.02} />
                  </linearGradient>
                </defs>
                <CartesianGrid strokeDasharray="3 3" className="stroke-border" />
                <XAxis dataKey="day" fontSize={12} />
                <YAxis yAxisId="l" fontSize={12} />
                <YAxis yAxisId="r" orientation="right" fontSize={12} />
                <Tooltip
                  formatter={(value, name) => {
                    const n = typeof value === 'number' ? value : Number(value ?? 0)
                    // 收入按金额显示，请求量按整数千分位——同一图两种量纲，共用格式会误读
                    return name === t('admin:kpiRevenue')
                      ? `$${n.toFixed(2)}`
                      : n.toLocaleString(i18n.language)
                  }}
                />
                <Legend />
                <Area
                  yAxisId="l"
                  type="monotone"
                  dataKey="requests"
                  name={t('admin:kpiRequests')}
                  stroke="var(--color-primary)"
                  fill="url(#g-req)"
                />
                <Area
                  yAxisId="r"
                  type="monotone"
                  dataKey="revenue"
                  name={t('admin:kpiRevenue')}
                  stroke="var(--color-success)"
                  fill="url(#g-rev)"
                />
              </AreaChart>
            </ResponsiveContainer>
          </div>
        )}
      </CardContent>
    </Card>
  )
}
