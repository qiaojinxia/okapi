import { useQuery } from '@tanstack/react-query'
import { getRouteApi, Link } from '@tanstack/react-router'
import dayjs from 'dayjs'
import { Activity, ArrowUpRight, Coins, Cpu, Gauge, LayoutDashboard, RefreshCw, ShieldCheck, Timer, Wallet, Zap } from 'lucide-react'
import { useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { ErrorState, LoadingState } from '@/components/ui/state'
import { Button } from '@/components/ui/button'
import { DateRangePicker } from '@/components/ui/date-range'
import { PageHeader } from '@/components/ui/page'
import { Segmented } from '@/components/ui/segmented'
import { UsageScope } from '@/components/usage-scope'
import { useUsageScope } from '@/hooks/use-usage-scope'
import { Stat } from '@/components/ui/stat'
import { QualityStrip } from '@/features/portal-overview/QualityStrip'
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
import { formatBp, formatCount, formatTokensPerSec } from '@/lib/money'
import { delta, formatDuration, formatUsd } from './format'
import { qk } from '@/lib/query-keys'

const routeApi = getRouteApi('/portal/')
/// 健康指标的图标一律中性色：颜色只留给状态（成功率的黄 / 红）。

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
  const { days = 7, view } = search
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
  // 吞吐量与管理端运营概览同一做法：全量口径不完整（窗口里有失败请求、按次计费的媒体请求等，它们没有可测的输出速度）
  // 但有实测样本时，显示样本值并注明样本数，而不是一律「—」
  const throughputSamples = total?.output_tps_samples ?? 0
  const partialThroughput = (total?.requests ?? 0) > 0 && total?.tokens_per_1k_sec == null
    && throughputSamples > 0 && total?.observed_output_tps_milli != null
  const throughput = partialThroughput ? total?.observed_output_tps_milli : total?.tokens_per_1k_sec
  const recentWalletSpend = q.isError || (range && q.data?.window?.end_date !== q.data?.window?.today) ? undefined : q.data?.wallet_window_spend_micro
  const refresh = async () => {
    setRefreshing(true)
    try { await Promise.all([q.refetch(), me.refetch()]) }
    finally { setRefreshing(false) }
  }

  // 上一段等长窗口：给三张主卡的「较前 N 天」。只在当前窗口回来之后查，口径与当前窗口一致
  const win = q.data?.window
  const span = q.data?.days ?? days
  const prevRange = win ? previousWindow(win.start_date, span) : null
  const prev = useQuery({
    queryKey: qk.myBreakdown(scope, span, `prev:${prevRange?.start}`),
    queryFn: () => apiFetch<BreakdownResp>(`/api/me/stats/breakdown?scope=${scope}&start_date=${prevRange!.start}&end_date=${prevRange!.end}`),
    enabled: usageScope.ready && prevRange !== null,
    retry: false,
    staleTime: 5 * 60_000,
  })
  const previous = prev.isError ? undefined : prev.data?.total

  // 旧链接里的 ?view=trend / models / tokens 曾是页签，现在滚到趋势或模型排行；只在首次数据回来时滚一次
  const scrolled = useRef(false)
  useEffect(() => {
    if (scrolled.current || !q.isSuccess || !view) return
    scrolled.current = true
    document.getElementById(view === 'trend' ? 'portal-trend' : 'portal-models')?.scrollIntoView({ block: 'start' })
  }, [q.isSuccess, view])

  const hit = total ? cacheHit(total) : null
  const failed = total?.errors

  const health = <QualityStrip label={t('charts:performance')} loading={loading} items={[
    {
      icon: ShieldCheck, accent: 'bg-success/12 text-success', label: t('charts:metric_success'),
      value: total?.success_rate_bp == null ? '—' : formatBp(total.success_rate_bp, locale),
      sub: total && total.requests > 0 && failed != null ? (failed > 0 ? t('portal:failedCount', { n: formatCount(failed, locale) }) : t('portal:noFailures')) : undefined,
      tone: total?.success_rate_bp == null ? undefined
        : total.success_rate_bp <= 9_500 ? 'bad' : total.success_rate_bp <= 9_900 ? 'warn' : 'good',
    },
    {
      icon: Gauge, accent: 'bg-chart-4/12 text-chart-4', label: t('charts:throughput'),
      value: throughput == null ? '—' : `${formatTokensPerSec(throughput, locale)} Token/s`,
      ...(partialThroughput
        ? {
            sub: t('portal:throughputMeasured', { n: formatCount(throughputSamples, locale), total: formatCount(total?.requests ?? 0, locale) }),
            subTitle: t('charts:throughputSampleHint'),
          }
        : { sub: t('portal:throughputCaption') }),
    },
    {
      icon: Zap, accent: 'bg-chart-3/12 text-chart-3', label: t('analytics:ttft'),
      value: total?.avg_ttft_ms == null ? '—' : formatDuration(total.avg_ttft_ms, locale),
      ...(q.isError
        ? { sub: t('charts:statisticsUnavailable') }
        : total?.avg_ttft_ms == null
          ? { sub: t('charts:ttftNoSamples'), subTitle: t('charts:ttftUnavailable') }
          : { sub: t('portal:ttftSamples', { count: total.ttft_samples ?? 0 }), subTitle: t('charts:ttftHint', { count: total.ttft_samples ?? 0 }) }),
    },
    {
      icon: Timer, accent: 'bg-chart-1/12 text-chart-1', label: t('charts:metric_latency'),
      value: total?.avg_latency_ms == null ? '—' : formatDuration(total.avg_latency_ms, locale),
      sub: t('portal:latencyCaption'),
    },
  ]} />

  return (
    <div className="flex min-w-0 flex-col gap-4">
      <PageHeader
        icon={LayoutDashboard}
        title={t('portal:dashboard')}
        meta={<LiveBadge live={live} scope={scope} />}
        action={<>
          <Button variant="outline" loading={refreshing} onClick={() => void refresh()}>
            {!refreshing && <RefreshCw className="h-4 w-4" />}{t('common:refresh')}
          </Button>
          <Link to="/portal/logs" search={logSearch} className="inline-flex min-h-10 items-center gap-2 rounded-md border border-border bg-card px-3 text-sm font-medium shadow-xs outline-none hover:bg-muted focus-visible:ring-2 focus-visible:ring-primary/40">
            {t('portal:usageDetails')}<ArrowUpRight className="h-4 w-4" />
          </Link>
        </>}
      />
      {/* 新用户优先看到下一步；已完成接入的账户直接展示数据。 */}
      <GettingStartedCard />
      <section aria-label={t('portal:dashboardFilters')} className="flex min-w-0 flex-wrap items-center justify-between gap-x-4 gap-y-2">
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
          <DateRangePicker today={q.data?.window?.today ?? new Date().toISOString().slice(0, 10)} value={range}
            onApply={(value) => void navigate({ search: (prev) => ({ ...prev, start_date: value.start, end_date: value.end }) })} />
        </div>
      </section>

      {/* 四个数字按用户先看什么排：用了多少 Token、调了多少次、还剩多少、花了多少。Token 用绿色最先抓住视线 */}
      <section aria-label={t('portal:dashboardMetrics')} className="grid min-w-0 grid-cols-1 gap-3 sm:grid-cols-2 xl:grid-cols-4">
        <Stat
          layout="stacked"
          icon={Cpu}
          iconClassName="bg-success/12 text-success"
          tone="good"
          label={t('common:tokens')}
          loading={loading}
          value={total ? formatCount(total.tokens, locale) : '—'}
          sub={<>
            <DeltaText value={total ? delta(total.tokens, previous?.tokens) : null} days={span} />
            {hit && <span>{hit.partial ? t('portal:cacheHitMeasured', { v: formatBp(hit.bp, locale), n: hit.samples, total: total?.requests ?? 0 })
              : t('portal:cacheHit', { v: hit.bp == null ? '—' : formatBp(hit.bp, locale) })}</span>}
          </>}
        />
        <Stat
          layout="stacked"
          icon={Activity}
          iconClassName="bg-chart-1/12 text-chart-1"
          label={t('common:requests')}
          loading={loading}
          value={total ? formatCount(total.requests, locale) : '—'}
          sub={<>
            <DeltaText value={total ? delta(total.requests, previous?.requests) : null} days={span} />
            {total && total.requests > 0 && <span>{t('portal:callsPerDay', { v: perDay(total.requests, span, locale) })}</span>}
          </>}
        />
        <Stat
          layout="stacked"
          icon={Wallet}
          iconClassName="bg-warning/14 text-warning"
          label={t('portal:accountBalance')}
          loading={me.isPending}
          value={me.data ? formatUsd(me.data.balance_micro, locale) : '—'}
          // 副行按"钱什么时候没"排优先级：到期清零日 vs 按日均烧完的那天，谁更近说谁；
          // 14 天内转黄、3 天内转红。有订阅时再挂一行订阅剩余（§11.28）——请求先扣它。
          sub={
            me.data && me.data.subscription_until_unix > 0 ? (
              <>
                <span>{balanceSub(me.data, recentWalletSpend, span, t)}</span>
                <Link to="/portal/plans" className="text-primary underline decoration-dotted">
                  {t('portal:balanceSubLine', { amount: formatUsd(me.data.subscription_remaining_micro, locale) })}
                </Link>
              </>
            ) : (
              balanceSub(me.data ?? null, recentWalletSpend, span, t)
            )
          }
          tone={balanceTone(me.data ?? null, recentWalletSpend, span)}
        />
        <Stat
          layout="stacked"
          icon={Coins}
          iconClassName="bg-chart-3/12 text-chart-3"
          label={t('portal:totalSpend')}
          loading={loading}
          value={total ? formatUsd(total.amount_micro, locale) : '—'}
          sub={<>
            <DeltaText value={total ? delta(total.amount_micro, previous?.amount_micro) : null} days={span} />
            {total && total.discount_micro > 0 && <span>{t('portal:savedLine', { amount: formatUsd(total.discount_micro, locale) })}</span>}
          </>}
        />
      </section>

      {health}

      {q.isError ? (
        <ErrorState message={describeError(q.error)} onRetry={() => void q.refetch()} />
      ) : q.isPending ? <LoadingState /> : q.data?.total && (
        <>
          <UsageOverview data={q.data} logSearch={logSearch} metric={search.measure ?? 'amount'}
            onMetricChange={(measure) => void navigate({ search: (prev) => ({ ...prev, measure }), resetScroll: false })} />
        </>
      )}
    </div>
  )
}

/// 当前窗口之前、等长的一段（起止都是日历日）。
function previousWindow(start: string, days: number): { start: string; end: string } | null {
  const first = dayjs(start)
  if (!first.isValid() || days < 1) return null
  return { start: first.subtract(days, 'day').format('YYYY-MM-DD'), end: first.subtract(1, 'day').format('YYYY-MM-DD') }
}

function perDay(n: number, days: number, locale: string): string {
  const v = n / Math.max(1, days)
  return v.toLocaleString(locale, { maximumFractionDigits: v >= 10 ? 0 : 1 })
}

/// 「较前 7 天 ▲12%」：上涨 / 下跌只用箭头和文字表达，不上色——花得多不一定是坏事。
function DeltaText({ value, days }: { value: number | null; days: number }) {
  const { t, i18n } = useTranslation()
  if (value === null) return null
  const pct = Math.abs(value * 100)
  const shown = pct >= 100 ? Math.round(pct).toLocaleString(i18n.language) : pct.toLocaleString(i18n.language, { maximumFractionDigits: pct < 10 ? 1 : 0 })
  const arrow = value > 0.0005 ? '▲' : value < -0.0005 ? '▼' : '·'
  return <span className="tabular-nums">{t('portal:periodChange', { days, delta: `${arrow} ${shown}%` })}</span>
}

/// 标题旁的实时速率：只有单把 key 视角有限流器读数。接近上限（≥80%）转黄、触顶转红。
function LiveBadge({ live, scope }: { live: BreakdownResp['live']; scope: Scope }) {
  const { t, i18n } = useTranslation()
  if (scope === 'user' || live === null) return null
  const locale = i18n.language
  const ratio = live.rpm_limit ? live.rpm / live.rpm_limit : 0
  const tone = ratio >= 1 ? 'border-destructive/40 text-destructive' : ratio >= 0.8 ? 'border-warning/40 text-warning' : 'border-border text-muted-foreground'
  const rpm = live.rpm_limit ? `${formatCount(live.rpm, locale)}/${formatCount(live.rpm_limit, locale)}` : formatCount(live.rpm, locale)
  const tpm = live.tpm_limit ? `${formatCount(live.tpm, locale)}/${formatCount(live.tpm_limit, locale)}` : formatCount(live.tpm, locale)
  return <span title={t('portal:liveHint')} className={`inline-flex items-center gap-1.5 rounded-full border px-2 py-0.5 text-xs tabular-nums ${tone}`}>
    <span aria-hidden className="h-1.5 w-1.5 rounded-full bg-success" />
    {t('portal:liveRate', { rpm, tpm })}
  </span>
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
