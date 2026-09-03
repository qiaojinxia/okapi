import { useQuery } from '@tanstack/react-query'
import { Activity, AlertTriangle, Coins, Cpu, Users } from 'lucide-react'
import type { LucideIcon } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { Stat } from '@/components/ui/stat'
import { ErrorState } from '@/components/ui/state'
import type { OverviewResp } from '@/features/dashboard/types'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatBp, formatCount, formatMoneyAggregate } from '@/lib/money'
import { qk } from '@/lib/query-keys'

/// 单张 KPI：主数字给今天，副行给窗口累计。
///
/// 为什么两个数字并排：只看今天无法判断高低（1485 次请求是多还是少？），
/// 只看累计又看不出当下状态。并排给出后不必去别的页面对照。
/// 外观交给统一的 `Stat`（门户看板 / 分析页同款），这里只保留"今天 + 双锚点"的语义。
function Kpi({
  icon,
  label,
  today,
  window,
  tone,
  loading,
}: {
  icon: LucideIcon
  label: string
  today: string
  window: React.ReactNode
  tone?: 'default' | 'warn' | 'bad'
  loading: boolean
}) {
  // 副行两个锚点各自成块、允许换行：五列布局下"昨日 $889.35 · 7天 $6,376"
  // 一行放不下，截断会把 7 天那个数吞掉一半，比换行糟得多（Stat 的 sub 自带 flex-wrap）
  return <Stat icon={icon} label={label} value={today} sub={window} tone={tone} loading={loading} />
}

/// 站点 KPI 一屏。落地先答"现在怎么样"，故放在最上方。
///
/// 副行是「昨日 X · 近 N 天 Y」双锚点：昨日给环比（今天是涨是跌），
/// 窗口给基线（这个量级正常吗）。昨日取整日聚合，前端文案不假装是同比。
export function KpiCards({ days }: { days: number }) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const q = useQuery({
    queryKey: qk.statsOverview(days),
    queryFn: () => apiFetch<OverviewResp>(`/admin/stats/overview?days=${days}`),
  })

  if (q.isError) {
    return <ErrorState message={describeError(q.error)} onRetry={() => void q.refetch()} />
  }

  const loading = q.isPending
  const today = q.data?.today
  const yday = q.data?.yesterday
  const win = q.data?.window
  // 两个锚点拆成两个 span，交给父级 flex-wrap 决定同行还是换行
  const compare = (y: string, w: string) => (
    <>
      <span>{t('admin:kpiYesterday', { value: y })}</span>
      <span>{t('admin:kpiWindow', { days, value: w })}</span>
    </>
  )
  const errorBp = today?.error_rate_bp ?? 0

  return (
    <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 xl:grid-cols-5">
      <Kpi
        icon={Activity}
        loading={loading}
        label={t('admin:kpiRequests')}
        today={formatCount(today?.requests ?? 0, locale)}
        window={compare(
          formatCount(yday?.requests ?? 0, locale),
          formatCount(win?.requests ?? 0, locale),
        )}
      />
      <Kpi
        icon={Coins}
        loading={loading}
        label={t('admin:kpiRevenue')}
        today={formatMoneyAggregate(today?.amount_micro ?? 0, locale)}
        window={compare(
          formatMoneyAggregate(yday?.amount_micro ?? 0, locale),
          formatMoneyAggregate(win?.amount_micro ?? 0, locale),
        )}
      />
      <Kpi
        icon={Cpu}
        loading={loading}
        label={t('admin:kpiTokens')}
        today={formatCount(today?.tokens ?? 0, locale)}
        window={compare(
          formatCount(yday?.tokens ?? 0, locale),
          formatCount(win?.tokens ?? 0, locale),
        )}
      />
      <Kpi
        icon={Users}
        loading={loading}
        label={t('admin:kpiActiveUsers')}
        today={formatCount(today?.active_users ?? 0, locale)}
        window={compare(
          formatCount(yday?.active_users ?? 0, locale),
          formatCount(win?.active_users ?? 0, locale),
        )}
      />
      <Kpi
        icon={AlertTriangle}
        loading={loading}
        label={t('admin:kpiErrorRate')}
        today={formatBp(errorBp, locale)}
        window={compare(
          formatBp(yday?.error_rate_bp ?? 0, locale),
          formatBp(win?.error_rate_bp ?? 0, locale),
        )}
        // 阈值与渠道健康卡一致：1% 起提醒，5% 起告警
        tone={errorBp >= 500 ? 'bad' : errorBp >= 100 ? 'warn' : 'default'}
      />
    </div>
  )
}
