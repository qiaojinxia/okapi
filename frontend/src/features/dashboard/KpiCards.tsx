import { useQuery } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { Activity, AlertTriangle, ArrowUpRight, Coins, Cpu, Users } from 'lucide-react'
import type { LucideIcon } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { DeltaChip, Stat } from '@/components/ui/stat'
import { ErrorState } from '@/components/ui/state'
import type { OverviewResp } from '@/features/dashboard/types'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatBp, formatCount, formatMoneyAggregate } from '@/lib/money'
import { qk } from '@/lib/query-keys'
import type { AnalyticsSearch } from '@/routes/admin.stats'

/// 主数字跟随统计口径，副行提供对照；使用统一 Stat 保持指标样式一致。
function Kpi({
  icon,
  label,
  shortLabel,
  today,
  window,
  tone,
  loading,
  search,
  detail,
}: {
  icon: LucideIcon
  label: string
  shortLabel: string
  today: string
  window: React.ReactNode
  tone?: 'default' | 'warn' | 'bad'
  loading: boolean
  search: AnalyticsSearch
  detail?: string
}) {
  const { t } = useTranslation()
  // 副行两个锚点各自成块、允许换行：五列布局下"昨日 $889.35 · 7天 $6,376"
  // 一行放不下，截断会把 7 天那个数吞掉一半，比换行糟得多（Stat 的 sub 自带 flex-wrap）
  const className = 'min-w-0 rounded-xl last:col-span-2 md:last:col-span-1'
  if (loading) return <Stat layout="stacked" compact className={className} icon={icon} label={label} value={today} sub={window} loading />
  return <Link to="/admin/stats" search={search} aria-label={t('admin:dashboardOpenMetric', { label, value: today })}
    className={`${className} group block outline-none focus-visible:ring-2 focus-visible:ring-primary/50 focus-visible:ring-offset-2 focus-visible:ring-offset-background`}>
    <Stat layout="stacked" compact className="h-full min-w-0 rounded-xl transition-colors group-hover:border-primary/40 group-hover:bg-accent/30 group-focus-visible:border-primary/40 lg:max-xl:px-2" icon={icon}
      label={<><span className="md:hidden">{shortLabel}</span><span className="hidden md:inline">{label}</span></>}
      value={<span className="flex min-w-0 flex-wrap items-center gap-x-1.5 text-lg sm:text-xl"><span className="min-w-0" title={today}>{today}</span>{detail && <span className="text-[11px] font-normal text-muted-foreground">{detail}</span>}<ArrowUpRight aria-hidden className="hidden h-3.5 w-3.5 shrink-0 text-muted-foreground group-hover:text-primary group-focus-visible:text-primary sm:block" /></span>}
      sub={window} tone={tone} />
  </Link>
}

/// 站点 KPI 一屏。落地先答"现在怎么样"，故放在最上方。
///
/// 副行是「昨日 X · 近 N 天 Y」双锚点：昨日给环比（今天是涨是跌），
/// 窗口给基线（这个量级正常吗）。昨日取整日聚合，前端文案不假装是同比。
export function KpiCards({ days, scope = 'today' }: { days: number; scope?: 'today' | 'window' }) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const q = useQuery({
    queryKey: qk.statsOverview(days),
    queryFn: () => apiFetch<OverviewResp>(`/admin/stats/overview?days=${days}`),
    retry: false,
  })

  if (q.isError) {
    return <ErrorState message={describeError(q.error)} onRetry={() => void q.refetch()} />
  }

  const loading = q.isPending
  const today = scope === 'today' ? q.data?.today : q.data?.window
  const yday = scope === 'today' ? q.data?.yesterday : q.data?.today
  const win = q.data?.window
  // 环比芯片打头 + 两个锚点各一个 span，交给父级 flex-wrap 决定同行还是换行
  const compare = (
    y: string,
    w: string,
    delta?: { current: number; previous: number; invert?: boolean },
  ) => (
    <>
      <span className="flex w-full flex-wrap items-center gap-x-1.5">
        {scope === 'today' && delta && (
          <DeltaChip
            current={delta.current}
            previous={delta.previous}
            invert={delta.invert}
            locale={locale}
          />
        )}
        <span title={scope === 'today' ? t('admin:kpiYesterday', { value: y }) : undefined}>{t(scope === 'today' ? 'admin:dashboardYesterdayShort' : 'admin:dashboardTodayValue', { value: y })}</span>
      </span>
      {scope === 'today' && <span className="w-full">{t('admin:kpiWindow', { days, value: w })}</span>}
    </>
  )
  const errorBp = today?.error_rate_bp ?? 0
  const period = scope === 'today' ? t('admin:kpiToday') : t('admin:lastDays', { days })
  const scopedLabel = (label: string) => t('admin:dashboardMetricLabel', { period, label })
  // 今日是自然日，不能沿用旁边趋势的 7/30 天窗口。
  const detailDays = scope === 'today' ? 1 : days

  return (
    <div aria-busy={loading} className="grid min-w-0 grid-cols-2 gap-2 md:grid-cols-3 lg:grid-cols-5">
      <Kpi
        icon={Activity}
        loading={loading}
        label={scopedLabel(t('admin:kpiRequests'))}
        shortLabel={t('admin:kpiRequests')}
        search={{ days: detailDays, measure: 'requests' }}
        today={formatCount(today?.requests ?? 0, locale)}
        window={compare(
          formatCount(yday?.requests ?? 0, locale),
          formatCount(win?.requests ?? 0, locale),
          { current: today?.requests ?? 0, previous: yday?.requests ?? 0 },
        )}
      />
      <Kpi
        icon={Coins}
        loading={loading}
        label={scopedLabel(t('admin:kpiRevenue'))}
        shortLabel={t('admin:kpiRevenue')}
        search={{ days: detailDays, measure: 'amount' }}
        today={formatMoneyAggregate(today?.amount_micro ?? 0, locale)}
        window={compare(
          formatMoneyAggregate(yday?.amount_micro ?? 0, locale),
          formatMoneyAggregate(win?.amount_micro ?? 0, locale),
          { current: today?.amount_micro ?? 0, previous: yday?.amount_micro ?? 0 },
        )}
      />
      <Kpi
        icon={Cpu}
        loading={loading}
        label={scopedLabel(t('admin:kpiTokens'))}
        shortLabel={t('admin:kpiTokens')}
        search={{ days: detailDays, measure: 'tokens' }}
        today={formatCount(today?.tokens ?? 0, locale)}
        window={compare(
          formatCount(yday?.tokens ?? 0, locale),
          formatCount(win?.tokens ?? 0, locale),
          { current: today?.tokens ?? 0, previous: yday?.tokens ?? 0 },
        )}
      />
      <Kpi
        icon={Users}
        loading={loading}
        label={scopedLabel(t('admin:kpiActiveUsers'))}
        shortLabel={t('admin:kpiActiveUsers')}
        search={{ days: detailDays, view: 'breakdown', by: 'user' }}
        today={formatCount(today?.active_users ?? 0, locale)}
        window={compare(
          formatCount(yday?.active_users ?? 0, locale),
          formatCount(win?.active_users ?? 0, locale),
          { current: today?.active_users ?? 0, previous: yday?.active_users ?? 0 },
        )}
      />
      <Kpi
        icon={AlertTriangle}
        loading={loading}
        label={scopedLabel(t('admin:kpiErrorRate'))}
        shortLabel={t('admin:kpiErrorRate')}
        search={{ days: detailDays, measure: 'error_rate' }}
        today={(today?.requests ?? 0) > 0 ? formatBp(errorBp, locale) : '—'}
        detail={(today?.requests ?? 0) > 0 ? t('admin:dashboardFailedCalls', { n: formatCount(today?.errors ?? 0, locale) }) : undefined}
        window={compare(
          (yday?.requests ?? 0) > 0 ? formatBp(yday?.error_rate_bp ?? 0, locale) : '—',
          (win?.requests ?? 0) > 0 ? formatBp(win?.error_rate_bp ?? 0, locale) : '—',
          // 错误率涨是坏事：极性反转，涨了涂红而不是涂绿
          (today?.requests ?? 0) > 0 && (yday?.requests ?? 0) > 0 ? { current: errorBp, previous: yday?.error_rate_bp ?? 0, invert: true } : undefined,
        )}
        // 阈值与渠道健康卡一致：1% 起提醒，5% 起告警
        tone={errorBp >= 500 ? 'bad' : errorBp >= 100 ? 'warn' : 'default'}
      />
    </div>
  )
}
