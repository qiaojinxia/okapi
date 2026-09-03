import { useQuery } from '@tanstack/react-query'
import dayjs from 'dayjs'
import { Activity, Coins, Cpu, Gauge, PiggyBank, Wallet } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { ErrorState } from '@/components/ui/state'
import { PageHeader } from '@/components/ui/page'
import { Segmented } from '@/components/ui/segmented'
import { Stat } from '@/components/ui/stat'
import { Tabs } from '@/components/ui/tabs'
import { ModelShareView } from '@/features/portal-overview/ModelShareView'
import { SpendTrendView } from '@/features/portal-overview/SpendTrendView'
import { TokenMixView } from '@/features/portal-overview/TokenMixView'
import type { BreakdownResp, Scope } from '@/features/portal-overview/types'
import { runwayDays } from '@/features/portal-overview/types'
import type { Me } from '@/hooks/use-auth'
import { useMe } from '@/hooks/use-auth'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatBp, formatCount, formatMoney } from '@/lib/money'
import { qk } from '@/lib/query-keys'

const VIEWS = ['trend', 'models', 'tokens'] as const
type View = (typeof VIEWS)[number]

/// 门户总览（对齐 new-api 数据看板的用户侧 + Sub2API 的 Token 构成）。
///
/// 一次查询（/api/me/stats/breakdown：day × model × token 四轴）喂全部视图：
/// 六张 KPI 常驻，三个页签只是同一份数据的不同切法——切签零请求。
/// 这与管理端统计页"每签一查询"不同：管理端各签打不同的 MV，这里只有一张。
export function PortalOverviewPage() {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const me = useMe()
  const [scope, setScope] = useState<Scope>('key')
  const [days, setDays] = useState(7)
  const [view, setView] = useState<View>('trend')

  const q = useQuery({
    queryKey: qk.myBreakdown(scope, days),
    queryFn: () =>
      apiFetch<BreakdownResp>(`/api/me/stats/breakdown?scope=${scope}&days=${days}`),
    // CH 未启用时 501 属预期；限流器计数每分钟翻桶，30s 刷一次够
    retry: false,
    refetchInterval: 30_000,
  })
  const total = q.data?.total
  const live = q.data?.live ?? null
  const loading = q.isPending

  const labels: Record<View, string> = {
    trend: t('portal:viewTrend'),
    models: t('portal:viewModels'),
    tokens: t('portal:viewTokens'),
  }

  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        title={t('portal:dashboard')}
        description={t('portal:dashboardDesc')}
        action={
          <>
            <Segmented
              ariaLabel={t('portal:logsScope')}
              value={scope}
              onChange={setScope}
              options={[
                { value: 'key', label: t('portal:scopeKey') },
                { value: 'user', label: t('portal:scopeUser') },
              ]}
            />
            <Segmented
              value={days}
              onChange={setDays}
              options={[7, 30, 90].map((d) => ({ value: d, label: t(`common:days_${d}`) }))}
            />
          </>
        }
      />

      <div className="grid grid-cols-2 gap-3 md:grid-cols-3 xl:grid-cols-6">
        <Stat
          icon={Wallet}
          label={t('common:balance')}
          loading={me.isPending}
          value={me.data ? formatMoney(me.data.balance_micro, locale) : '—'}
          // 副行按"钱什么时候没"排优先级：到期清零日 vs 按日均烧完的那天，谁更近说谁；
          // 两者都没有才退回分组文案。14 天内转黄、3 天内转红。
          sub={balanceSub(me.data ?? null, q.data?.wallet_window_spend_micro, days, t)}
          tone={balanceTone(me.data ?? null, q.data?.wallet_window_spend_micro, days)}
        />
        <Stat
          icon={Coins}
          label={t('portal:totalSpend')}
          loading={loading}
          value={total ? formatMoney(total.amount_micro, locale) : '—'}
          sub={t('portal:kpiWindow', { days })}
        />
        <Stat
          icon={PiggyBank}
          label={t('portal:saved')}
          loading={loading}
          value={total ? formatMoney(total.discount_micro, locale) : '—'}
          sub={t('portal:savedHint')}
          tone={total && total.discount_micro > 0 ? 'good' : 'default'}
        />
        <Stat
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
          icon={Cpu}
          label={t('common:tokens')}
          loading={loading}
          value={total ? formatCount(total.tokens, locale) : '—'}
          sub={total ? t('portal:cacheHit', { v: formatBp(total.cache_hit_bp, locale) }) : ''}
        />
        <LiveRateKpi
          live={live}
          scope={scope}
          loading={loading}
          avgTpmMicro={total?.avg_tpm_micro ?? 0}
        />
      </div>

      <Tabs
        items={VIEWS.map((id) => ({ id, label: labels[id] }))}
        active={view}
        onChange={(id) => setView(id as View)}
      />
      {q.isError ? (
        <ErrorState message={describeError(q.error)} onRetry={() => void q.refetch()} />
      ) : (
        <>
          {view === 'trend' && <SpendTrendView rows={q.data?.data ?? []} />}
          {view === 'models' && <ModelShareView rows={q.data?.data ?? []} />}
          {view === 'tokens' && <TokenMixView rows={q.data?.data ?? []} total={total ?? null} />}
        </>
      )}
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
  if (me.balance_micro <= 0) return { kind: 'depleted', days: 0 }
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
  avgTpmMicro: number
}) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  if (scope === 'user' || live === null) {
    return (
      <Stat
        icon={Gauge}
        label={t('portal:avgTpm')}
        loading={loading}
        value={fmtMicroRate(avgTpmMicro, locale)}
        sub={t('portal:avgTpmHint')}
      />
    )
  }
  const ratio = live.rpm_limit ? live.rpm / live.rpm_limit : 0
  return (
    <Stat
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
