import { useQuery } from '@tanstack/react-query'
import { Activity, AlertTriangle, Coins, Cpu, Users } from 'lucide-react'
import type { LucideIcon } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { Card, CardContent } from '@/components/ui/card'
import { ErrorState } from '@/components/ui/state'
import type { OverviewResp } from '@/features/dashboard/types'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatBp, formatCount, formatMoneyAggregate } from '@/lib/money'
import { cn } from '@/lib/utils'
import { qk } from '@/lib/query-keys'

/// 单张 KPI：主数字给今天，副行给窗口累计。
///
/// 为什么两个数字并排：只看今天无法判断高低（1485 次请求是多还是少？），
/// 只看累计又看不出当下状态。并排给出后不必去别的页面对照。
function Kpi({
  icon: Icon,
  label,
  today,
  window,
  tone,
}: {
  icon: LucideIcon
  label: string
  today: string
  window: string
  tone?: 'default' | 'warn' | 'bad'
}) {
  return (
    <Card>
      <CardContent className="flex items-start gap-3 py-4">
        <span
          className={cn(
            'mt-0.5 rounded-md p-2',
            tone === 'bad'
              ? 'bg-destructive/10 text-destructive'
              : tone === 'warn'
                ? 'bg-warning/10 text-warning'
                : 'bg-primary/10 text-primary',
          )}
        >
          <Icon className="h-4 w-4" />
        </span>
        <div className="flex min-w-0 flex-col">
          <span className="text-xs text-muted-foreground">{label}</span>
          <span className="truncate text-lg font-semibold">{today}</span>
          <span className="truncate text-xs text-muted-foreground">{window}</span>
        </div>
      </CardContent>
    </Card>
  )
}

/// 站点 KPI 一屏。落地先答"现在怎么样"，故放在最上方。
export function KpiCards({ days }: { days: number }) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const q = useQuery({
    queryKey: qk.statsOverview(days),
    queryFn: () => apiFetch<OverviewResp>(`/admin/stats/overview?days=${days}`),
  })

  if (q.isError) {
    return <ErrorState message={describeError(q.error)} />
  }

  const today = q.data?.today
  const win = q.data?.window
  const since = (v: string) => t('admin:kpiWindow', { days, value: v })
  const errorBp = today?.error_rate_bp ?? 0

  return (
    <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 xl:grid-cols-5">
      <Kpi
        icon={Activity}
        label={t('admin:kpiRequests')}
        today={formatCount(today?.requests ?? 0, locale)}
        window={since(formatCount(win?.requests ?? 0, locale))}
      />
      <Kpi
        icon={Coins}
        label={t('admin:kpiRevenue')}
        today={formatMoneyAggregate(today?.amount_micro ?? 0, locale)}
        window={since(formatMoneyAggregate(win?.amount_micro ?? 0, locale))}
      />
      <Kpi
        icon={Cpu}
        label={t('admin:kpiTokens')}
        today={formatCount(today?.tokens ?? 0, locale)}
        window={since(formatCount(win?.tokens ?? 0, locale))}
      />
      <Kpi
        icon={Users}
        label={t('admin:kpiActiveUsers')}
        today={formatCount(today?.active_users ?? 0, locale)}
        window={since(formatCount(win?.active_users ?? 0, locale))}
      />
      <Kpi
        icon={AlertTriangle}
        label={t('admin:kpiErrorRate')}
        today={formatBp(errorBp, locale)}
        window={since(formatBp(win?.error_rate_bp ?? 0, locale))}
        // 阈值与渠道健康卡一致：1% 起提醒，5% 起告警
        tone={errorBp >= 500 ? 'bad' : errorBp >= 100 ? 'warn' : 'default'}
      />
    </div>
  )
}
