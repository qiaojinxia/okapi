import { useQuery } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatBp, formatCount, formatMoney } from '@/lib/money'
import { qk } from '@/lib/query-keys'

export interface OverviewBucket {
  requests: number
  errors: number
  error_rate_bp: number
  tokens: number
  amount_micro: number
  discount_micro: number
  upstream_cost_micro: number
  margin_micro: number
  margin_rate_bp: number
  active_users: number
}


/// 站点 KPI 概览（今日 / 窗口双档）。
/// 与营收卡的分工：这里是单屏数字（含活跃用户数），营收卡是按日明细曲线。
export function OverviewCard({ days }: { days: number }) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const data = useQuery({
    queryKey: qk.statsOverview(days),
    queryFn: () =>
      apiFetch<{ today: OverviewBucket; window: OverviewBucket }>(
        `/admin/stats/overview?days=${days}`,
      ),
  })

  if (data.isError) {
    return <p className="text-sm text-destructive">{describeError(data.error)}</p>
  }

  const cells = (b: OverviewBucket | undefined) =>
    [
      [t('common:requests'), formatCount(b?.requests ?? 0, locale)],
      [t('admin:activeUsers'), formatCount(b?.active_users ?? 0, locale)],
      [t('common:amount'), formatMoney(b?.amount_micro ?? 0, locale)],
      [t('admin:margin'), formatMoney(b?.margin_micro ?? 0, locale)],
      [t('admin:errorRate'), formatBp(b?.error_rate_bp ?? 0, locale)],
    ] as const

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('admin:overviewKpi')}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-4">
        {(
          [
            [t('admin:kpiToday'), data.data?.today],
            [t('admin:kpiWindow', { days }), data.data?.window],
          ] as const
        ).map(([label, bucket]) => (
          <div key={label} className="flex flex-col gap-1.5">
            <span className="text-xs text-muted-foreground">{label}</span>
            <div className="grid grid-cols-2 gap-3 sm:grid-cols-5">
              {cells(bucket).map(([k, v]) => (
                <div key={k} className="flex flex-col">
                  <span className="text-xs text-muted-foreground">{k}</span>
                  <span className="text-lg font-bold">{v}</span>
                </div>
              ))}
            </div>
          </div>
        ))}
      </CardContent>
    </Card>
  )
}
