import { useQuery } from '@tanstack/react-query'
import { getRouteApi, Link } from '@tanstack/react-router'
import dayjs from 'dayjs'
import { Activity, ArrowUpRight, Coins, Cpu, Gauge, LayoutDashboard, PiggyBank, RefreshCw, Timer, Wallet, Zap } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { ErrorState, LoadingState } from '@/components/ui/state'
import { Button } from '@/components/ui/button'
import { DateRangePicker } from '@/components/ui/date-range'
import { PageHeader } from '@/components/ui/page'
import { Segmented } from '@/components/ui/segmented'
import { UsageScope } from '@/components/usage-scope'
import { useUsageScope } from '@/hooks/use-usage-scope'
import { Stat } from '@/components/ui/stat'
import { Tabs } from '@/components/ui/tabs'
import { ModelShareView } from '@/features/portal-overview/ModelShareView'
import { SpendTrendView } from '@/features/portal-overview/SpendTrendView'
import { TokenMixView } from '@/features/portal-overview/TokenMixView'
import { UsageOverview } from '@/features/portal-overview/UsageOverview'
import { cacheHit } from '@/features/portal-overview/cache-metrics'
import type { BreakdownResp, Scope } from '@/features/portal-overview/types'
import { runwayDays } from '@/features/portal-overview/types'
import { GettingStartedCard } from '@/features/portal-guide/GettingStartedCard'
import type { PortalLogSearch } from '@/features/logs/search'
import type { Me } from '@/hooks/use-auth'
import { useMe } from '@/hooks/use-auth'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatBp, formatCount, formatMoney, formatTokensPerSec } from '@/lib/money'
import { qk } from '@/lib/query-keys'
import { PORTAL_VIEWS } from './search'
import type { PortalView } from './search'

const routeApi = getRouteApi('/portal/')

/// 门户总览（对齐 new-api 数据看板的用户侧 + Sub2API 的 Token 构成）。
///
/// 一次查询（/api/me/stats/breakdown：day × model × token 四轴）喂全部视图：
/// 六张 KPI 常驻，默认综合视图展示趋势和构成，明细页签复用数据，切签零请求。
/// 这与管理端统计页"每签一查询"不同：管理端各签打不同的 MV，这里只有一张。
export function PortalOverviewPage() {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const me = useMe()
  const search = routeApi.useSearch()
  const navigate = routeApi.useNavigate()
  const { days = 7, view = 'overview' } = search
  const usageScope = useUsageScope(search.scope)
  const { scope } = usageScope
  const range = search.start_date && search.end_date ? { start: search.start_date, end: search.end_date } : null
  const [refreshing, setRefreshing] = useState(false)
  const rangeParams = range ? `&start_date=${range.start}&end_date=${range.end}` : ''

  const q = useQuery({
    queryKey: qk.myBreakdown(scope, days, rangeParams),
    queryFn: () =>
      apiFetch<BreakdownResp>(`/api/me/stats/breakdown?scope=${scope}&days=${days}${rangeParams}`),
    // CH 未启用时 501 属预期；限流器计数每分钟翻桶，30s 刷一次够
    retry: false,
    enabled: usageScope.ready,
    refetchInterval: 30_000,
  })
  const total = q.isError ? undefined : q.data?.total
  const live = q.isError ? null : q.data?.live ?? null
  const logWindow = q.isError ? undefined : q.data?.window
  const logSearch: PortalLogSearch = {
    scope,
    start_date: logWindow?.start_date,
    end_date: logWindow?.end_date,
    timezone: logWindow?.timezone,
  }
  const loading = q.isPending
  const recentWalletSpend = q.isError || (range && q.data?.window?.end_date !== q.data?.window?.today) ? undefined : q.data?.wallet_window_spend_micro
  const refresh = async () => {
    setRefreshing(true)
    try { await Promise.all([q.refetch(), me.refetch()]) }
    finally { setRefreshing(false) }
  }

  const labels: Record<PortalView, string> = {
    overview: t('portal:viewOverview'),
    trend: t('portal:viewTrend'),
    models: t('portal:viewModels'),
    tokens: t('portal:viewTokens'),
  }

  return (
    <div className="flex min-w-0 flex-col gap-3">
      <PageHeader
        icon={LayoutDashboard}
        title={t('portal:dashboard')}
        action={<>
          <Button variant="outline" loading={refreshing} onClick={() => void refresh()}>
            {!refreshing && <RefreshCw className="h-4 w-4" />}{t('common:refresh')}
          </Button>
          <Link to="/portal/logs" search={logSearch} className="inline-flex min-h-10 items-center gap-2 rounded-md bg-primary px-3 text-sm font-medium text-primary-foreground shadow-xs outline-none hover:bg-primary/90 focus-visible:ring-2 focus-visible:ring-primary/40">
            {t('portal:usageDetails')}<ArrowUpRight className="h-4 w-4" />
          </Link>
        </>}
      />
      {/* 新用户优先看到下一步；已完成接入的账户直接展示数据。 */}
      <GettingStartedCard />
      <section aria-label={t('portal:dashboardFilters')} className="flex min-w-0 flex-wrap items-center justify-between gap-3 rounded-xl border border-border bg-card px-3 py-2">
          <UsageScope {...usageScope} onChange={(value) => {
            usageScope.setScope(value)
            void navigate({ search: (prev) => ({ ...prev, scope: value }) })
          }} />
          <div className="flex min-w-0 max-w-full flex-wrap items-center gap-2">
            <Segmented
              ariaLabel={t('charts:period')}
              value={range ? 0 : days}
              onChange={(value) => void navigate({ search: (prev) => ({ ...prev, days: value, start_date: undefined, end_date: undefined }) })}
              options={[1, 7, 30, 90].map((d) => ({ value: d, label: d === 1 ? t('charts:today') : t(`common:days_${d}`) }))}
            />
          </div>
          <DateRangePicker today={q.data?.window?.today ?? new Date().toISOString().slice(0, 10)} value={range}
            onApply={(value) => void navigate({ search: (prev) => ({ ...prev, start_date: value.start, end_date: value.end }) })} />
      </section>

      <section aria-label={t('portal:dashboardMetrics')} className="grid min-w-0 grid-cols-2 gap-3 md:grid-cols-3 xl:grid-cols-6">
        <Stat
          compact
          layout="stacked"
          icon={Wallet}
          label={t('portal:accountBalance')}
          loading={me.isPending}
          value={me.data ? formatMoney(me.data.balance_micro, locale) : '—'}
          // 副行按"钱什么时候没"排优先级：到期清零日 vs 按日均烧完的那天，谁更近说谁；
          // 两者都没有才退回分组文案。14 天内转黄、3 天内转红。
          // 有订阅时再挂一行订阅剩余（§11.28）——请求先扣它，钱包数字单看会误导。
          sub={
            me.data && me.data.subscription_until_unix > 0 ? (
              <>
                <span>{balanceSub(me.data, recentWalletSpend, q.data?.days ?? days, t)}</span>
                <Link to="/portal/plans" className="text-primary underline decoration-dotted">
                  {t('portal:balanceSubLine', {
                    amount: formatMoney(me.data.subscription_remaining_micro, locale),
                  })}
                </Link>
              </>
            ) : (
              balanceSub(me.data ?? null, recentWalletSpend, q.data?.days ?? days, t)
            )
          }
          tone={balanceTone(me.data ?? null, recentWalletSpend, q.data?.days ?? days)}
        />
        <Stat
          compact
          layout="stacked"
          icon={Coins}
          label={t('portal:totalSpend')}
          loading={loading}
          value={total ? formatMoney(total.amount_micro, locale) : '—'}
          sub={range ? `${range.start} — ${range.end}` : t('portal:kpiWindow', { days: q.data?.days ?? days })}
        />
        <Stat
          compact
          layout="stacked"
          icon={PiggyBank}
          label={t('portal:saved')}
          loading={loading}
          value={total ? formatMoney(total.discount_micro, locale) : '—'}
          sub={t('portal:savedHint')}
          tone={total && total.discount_micro > 0 ? 'good' : 'default'}
        />
        <Stat
          compact
          layout="stacked"
          icon={Activity}
          label={t('common:requests')}
          loading={loading}
          value={total ? formatCount(total.requests, locale) : '—'}
          sub={
            total
              ? t('portal:avgRpm', { v: fmtMicroRate(total.avg_rpm_micro, locale) })
              : ''
          }
        />
        <Stat
          compact
          layout="stacked"
          icon={Cpu}
          label={t('common:tokens')}
          loading={loading}
          value={total ? formatCount(total.tokens, locale) : '—'}
          sub={total ? (() => {
            const hit = cacheHit(total)
            return hit.partial ? t('portal:cacheHitMeasured', { v: formatBp(hit.bp, locale), n: hit.samples, total: total.requests })
              : t('portal:cacheHit', { v: hit.bp == null ? '—' : formatBp(hit.bp, locale) })
          })() : ''}
        />
        <LiveRateKpi
          live={live}
          scope={scope}
          loading={loading}
          avgTpmMicro={total?.avg_tpm_micro}
        />
      </section>

      <section className="grid min-w-0 grid-cols-2 gap-3 xl:grid-cols-4" aria-label={t('charts:performance')}>
        <Stat compact layout="stacked" icon={Zap} label={t('analytics:ttft')} loading={loading}
          value={total?.avg_ttft_ms == null ? '—' : `${formatCount(total.avg_ttft_ms, locale)} ms`}
          sub={q.isError ? t('charts:statisticsUnavailable') : total?.avg_ttft_ms == null ? t('charts:ttftUnavailable') : t('charts:ttftHint', { count: total.ttft_samples ?? 0 })} />
        <Stat compact layout="stacked" icon={Timer} label={t('charts:metric_latency')} loading={loading}
          value={total?.avg_latency_ms == null ? '—' : `${formatCount(total.avg_latency_ms, locale)} ms`} />
        <Stat compact layout="stacked" icon={Activity} label={t('charts:metric_success')} loading={loading}
          value={total?.success_rate_bp == null ? '—' : formatBp(total.success_rate_bp, locale)} />
        <Stat compact layout="stacked" icon={Gauge} label={t('charts:throughput')} loading={loading}
          value={total?.tokens_per_1k_sec == null ? '—' : `${formatTokensPerSec(total.tokens_per_1k_sec, locale)} Token/s`} />
      </section>

      <Tabs
        id="portal-views"
        ariaLabel={t('portal:dashboardViews')}
        items={PORTAL_VIEWS.map((id) => ({ id, label: labels[id], panelId: `portal-view-${id}` }))}
        active={view}
        onChange={(id) => void navigate({ search: (prev) => ({ ...prev, view: id as PortalView }) })}
      />
      <div id={`portal-view-${view}`} role="tabpanel" aria-labelledby={`portal-views-${view}`} tabIndex={0} className="min-w-0 rounded-xl outline-none focus-visible:ring-2 focus-visible:ring-primary/40">
      {q.isError ? (
        <ErrorState message={describeError(q.error)} onRetry={() => void q.refetch()} />
      ) : q.isPending ? <LoadingState /> : (
        <>
          {view === 'overview' && q.data?.total && <UsageOverview data={q.data} logSearch={logSearch} metric={search.measure ?? 'amount'} onView={(next) => void navigate({ search: (prev) => ({ ...prev, view: next }) })} />}
          {view === 'trend' && <SpendTrendView rows={q.data?.data ?? []} days={q.data?.days ?? days} window={q.data?.window} metric={search.measure ?? 'amount'} onMetricChange={(measure) => void navigate({ search: (prev) => ({ ...prev, measure }) })} />}
          {view === 'models' && <ModelShareView rows={q.data?.data ?? []} logSearch={logSearch}
            metric={search.model_measure ?? 'amount'} onMetricChange={(model_measure) => void navigate({ search: (prev) => ({ ...prev, model_measure }) })}
            query={search.model_query ?? ''} onQueryChange={(model_query) => void navigate({ search: (prev) => ({ ...prev, model_query: model_query || undefined }), replace: true })} />}
          {view === 'tokens' && <TokenMixView rows={q.data?.data ?? []} total={total ?? null} />}
        </>
      )}
      </div>
    </div>
  )
}

/// 余额"还能活几天"：取到期清零与按日均烧完两者中更近的一个；都没有 → null。
function balanceHorizon(
  me: Me | null,
  walletSpend: number | undefined,
  days: number,
): { kind: 'expiry' | 'runway' | 'depleted'; days: number } | null {
  if (me === null) return null
  // 订阅池还有额度时钱包为 0 不算"没钱"：请求先扣订阅池
  if (me.balance_micro <= 0 && me.subscription_remaining_micro <= 0) return { kind: 'depleted', days: 0 }
  if (me.balance_micro <= 0) return null
  const expiry = me.balance_expires_at ? dayjs(me.balance_expires_at).diff(dayjs(), 'day', true) : null
  const runway = walletSpend === undefined ? null : runwayDays(me.balance_micro, walletSpend, days)
  if (expiry !== null && (runway === null || expiry <= runway)) return { kind: 'expiry', days: expiry }
  if (runway !== null) return { kind: 'runway', days: runway }
  return null
}

function balanceTone(me: Me | null, walletSpend: number | undefined, days: number): 'warn' | 'bad' | 'default' {
  const h = balanceHorizon(me, walletSpend, days)
  if (h === null) return 'default'
  if (h.days <= 3) return 'bad'
  if (h.days <= 14) return 'warn'
  return 'default'
}

function balanceSub(
  me: Me | null,
  walletSpend: number | undefined,
  days: number,
  t: (key: string, opts?: Record<string, unknown>) => string,
): string {
  if (me === null) return ''
  const h = balanceHorizon(me, walletSpend, days)
  if (h === null) return `${t('logs:group')} ${me.group}`
  if (h.kind === 'depleted') return t('portal:balanceDepleted')
  if (h.kind === 'expiry') {
    return t('portal:balanceExpires', { date: dayjs(me.balance_expires_at).format('YYYY-MM-DD') })
  }
  if (h.days < 1) return t('portal:runwayUnderDay')
  if (h.days > 999) return t('portal:runwayLong')
  return t('portal:runwayDays', { n: Math.floor(h.days), days })
}

/// 百万分位速率 → 人读的数：≥1 给一位小数，<1 给三位（0.007/min 这种量级
/// 是个人用户的常态，两位小数会显示成 0.00）。
function fmtMicroRate(micro: number, locale: string): string {
  const v = micro / 1_000_000
  return v.toLocaleString(locale, { maximumFractionDigits: v >= 1 ? 1 : 3 })
}

/// 当前速率卡：key 视角给限流器视角的**本分钟 RPM / 上限**（老 ok-api 用户页取法
/// + 对照上限——直接回答"我离限流还有多远"）；汇总视角没有单一上限，
/// 退回 new-api 的窗口平均 TPM。接近上限（≥80%）转黄、触顶转红。
function LiveRateKpi({
  live,
  scope,
  loading,
  avgTpmMicro,
}: {
  live: BreakdownResp['live']
  scope: Scope
  loading: boolean
  avgTpmMicro: number | undefined
}) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  if (scope === 'user' || live === null) {
    return (
      <Stat
        compact
        layout="stacked"
        icon={Gauge}
        label={t('portal:avgTpm')}
        loading={loading}
        value={avgTpmMicro === undefined ? '—' : fmtMicroRate(avgTpmMicro, locale)}
        sub={t('portal:avgTpmHint')}
      />
    )
  }
  const ratio = live.rpm_limit ? live.rpm / live.rpm_limit : 0
  return (
    <Stat
      compact
      layout="stacked"
      icon={Gauge}
      label={t('portal:liveRpm')}
      loading={loading}
      value={
        live.rpm_limit
          ? `${formatCount(live.rpm, locale)} / ${formatCount(live.rpm_limit, locale)}`
          : formatCount(live.rpm, locale)
      }
      sub={
        live.tpm_limit
          ? t('portal:liveTpmCapped', {
              v: formatCount(live.tpm, locale),
              cap: formatCount(live.tpm_limit, locale),
            })
          : t('portal:liveTpm', { v: formatCount(live.tpm, locale) })
      }
      tone={ratio >= 1 ? 'bad' : ratio >= 0.8 ? 'warn' : 'default'}
    />
  )
}
