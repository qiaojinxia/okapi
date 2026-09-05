import { Activity, AlertTriangle, Coins, Cpu, Database, Timer } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { Stat } from '@/components/ui/stat'
import type { CubeMetrics } from '@/features/analytics/types'
import { formatBp, formatCount, formatMoneyAggregate } from '@/lib/money'
import { cn } from '@/lib/utils'

/// 环比：当前 vs 上一同长窗口。`invert` 表示"涨是坏事"（错误率 / 时延）。
///
/// 比率类指标给百分点差（"错误率 1.2% → 3.5%"看的是 +2.3pp，不是 +190%），
/// 计数类给百分比。上期为 0 时不给数——除以零后的"∞%"没有信息量。
function Delta({
  cur,
  prev,
  kind,
  invert,
  locale,
}: {
  cur: number
  prev: number | undefined
  kind: 'count' | 'bp'
  invert?: boolean
  locale: string
}) {
  const { t } = useTranslation()
  if (prev === undefined) return null
  let text: string
  let up: boolean
  if (kind === 'bp') {
    const pp = (cur - prev) / 100
    if (Math.abs(pp) < 0.05) return <span>{t('analytics:deltaFlat')}</span>
    up = pp > 0
    text = `${up ? '+' : ''}${pp.toLocaleString(locale, { maximumFractionDigits: 1 })}pp`
  } else {
    if (prev <= 0) {
      return cur > 0 ? <span>{t('analytics:deltaNew')}</span> : null
    }
    const pct = ((cur - prev) / prev) * 100
    if (Math.abs(pct) < 0.5) return <span>{t('analytics:deltaFlat')}</span>
    up = pct > 0
    text = `${up ? '+' : ''}${pct.toLocaleString(locale, { maximumFractionDigits: 0 })}%`
  }
  const good = invert ? !up : up
  return (
    <span className={cn('font-medium tabular-nums', good ? 'text-success' : 'text-destructive')}>
      {up ? '▲' : '▼'} {text}
    </span>
  )
}

/// KPI 卡里的金额：六列布局下每张卡只放得下十来个字符，"$71,535.31"会被截成
/// "$71,535…"——万元以上转紧凑记法（$71.5K），精确值在拆分表与 tooltip 里。
function kpiMoney(micro: number, locale: string): string {
  const usd = micro / 1_000_000
  if (Math.abs(usd) >= 10_000) {
    return new Intl.NumberFormat(locale, {
      style: 'currency',
      currency: 'USD',
      notation: 'compact',
      maximumFractionDigits: 1,
    }).format(usd)
  }
  return formatMoneyAggregate(micro, locale)
}

/// 过滤后的六项 KPI（请求 / 消费 / Tokens / 错误率 / 缓存命中 / 平均时延），
/// 每项副行是与上一同长窗口的环比——这是这页替代旧"今日 / 窗口"双档的地方：
/// 过滤到某个用户或渠道后"今日"没有环比意义，"比上个 7 天涨了 40%"才有。
export function KpiStrip({
  total,
  previous,
  days,
  loading,
}: {
  total: Partial<CubeMetrics> | undefined
  previous: Partial<CubeMetrics> | undefined
  days: number
  loading: boolean
}) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  if (!total && !loading) return <div className="grid grid-cols-2 gap-3 md:grid-cols-3 xl:grid-cols-6">{[
    { icon: Activity, label: t('common:requests') }, { icon: Coins, label: t('analytics:kpiSpend') }, { icon: Cpu, label: t('common:tokens') },
    { icon: AlertTriangle, label: t('admin:errorRate') }, { icon: Database, label: t('analytics:kpiCacheHit') }, { icon: Timer, label: t('analytics:kpiLatency') },
  ].map((item) => <Stat key={item.label} {...item} layout="stacked" value="—" />)}</div>
  const cur = total ?? {}
  const prev = previous ?? {}
  const vsPrev = <span>{t('analytics:vsPrevious', { days })}</span>
  const errBp = cur.error_rate_bp ?? 0

  return (
    <div className="grid grid-cols-2 gap-3 md:grid-cols-3 xl:grid-cols-6">
      <Stat
        layout="stacked"
        icon={Activity}
        loading={loading}
        label={t('common:requests')}
        value={formatCount(cur.requests ?? 0, locale)}
        sub={
          <>
            <Delta cur={cur.requests ?? 0} prev={prev.requests} kind="count" locale={locale} />
            {vsPrev}
          </>
        }
      />
      <Stat
        layout="stacked"
        icon={Coins}
        loading={loading}
        label={t('analytics:kpiSpend')}
        value={kpiMoney(cur.amount_micro ?? 0, locale)}
        sub={
          <>
            <Delta cur={cur.amount_micro ?? 0} prev={prev.amount_micro} kind="count" locale={locale} />
            {cur.known_margin_micro != null ? (
              <span title={t('analysis:costHint')} className={cn(cur.known_margin_micro < 0 && 'text-destructive')}>
                {t('analysis:coveredMargin')} {formatMoneyAggregate(cur.known_margin_micro, locale)} · {t('analysis:coverage', { v: formatBp(cur.cost_coverage_bp ?? 0, locale) })}
              </span>
            ) : (cur.discount_micro ?? 0) > 0 ? (
              <span>{t('analytics:kpiSaved', { v: formatMoneyAggregate(cur.discount_micro ?? 0, locale) })}</span>
            ) : (
              vsPrev
            )}
          </>
        }
      />
      <Stat
        layout="stacked"
        icon={Cpu}
        loading={loading}
        label={t('common:tokens')}
        value={formatCount(cur.tokens ?? 0, locale)}
        sub={
          <>
            <Delta cur={cur.tokens ?? 0} prev={prev.tokens} kind="count" locale={locale} />
            <span>
              {t('analytics:kpiTokenMix', {
                i: formatCount(cur.prompt_tokens ?? 0, locale),
                o: formatCount(cur.completion_tokens ?? 0, locale),
              })}
            </span>
          </>
        }
      />
      <Stat
        layout="stacked"
        icon={AlertTriangle}
        loading={loading}
        label={t('admin:errorRate')}
        value={(cur.requests ?? 0) > 0 ? formatBp(errBp, locale) : '—'}
        tone={errBp >= 500 ? 'bad' : errBp >= 100 ? 'warn' : 'default'}
        sub={
          <>
            {(cur.requests ?? 0) > 0 && (prev.requests ?? 0) > 0 && <Delta cur={errBp} prev={prev.error_rate_bp} kind="bp" invert locale={locale} />}
            <span>{t('analytics:kpiErrors', { n: formatCount(cur.errors ?? 0, locale) })}</span>
          </>
        }
      />
      <Stat
        layout="stacked"
        icon={Database}
        loading={loading}
        label={t('analytics:kpiCacheHit')}
        value={(cur.prompt_tokens ?? 0) > 0 ? formatBp(cur.cache_hit_bp ?? 0, locale) : '—'}
        sub={
          <>
            {(cur.prompt_tokens ?? 0) > 0 && (prev.prompt_tokens ?? 0) > 0 && <Delta cur={cur.cache_hit_bp ?? 0} prev={prev.cache_hit_bp} kind="bp" locale={locale} />}
            <span>{t('analytics:kpiCached', { n: formatCount(cur.cached_tokens ?? 0, locale) })}</span>
          </>
        }
      />
      <Stat
        layout="stacked"
        icon={Timer}
        loading={loading}
        label={t('analytics:kpiLatency')}
        value={(cur.requests ?? 0) > 0 && cur.avg_latency_ms != null ? `${formatCount(cur.avg_latency_ms, locale)} ms` : '—'}
        sub={
          <>
            {(cur.requests ?? 0) > 0 && (prev.requests ?? 0) > 0 && <Delta
              cur={cur.avg_latency_ms ?? 0}
              prev={prev.avg_latency_ms}
              kind="count"
              invert
              locale={locale}
            />}
            {(cur.avg_ttft_ms ?? 0) > 0 ? (
              <span>{t('analytics:kpiTtft', { v: formatCount(cur.avg_ttft_ms ?? 0, locale) })}</span>
            ) : (
              vsPrev
            )}
          </>
        }
      />
    </div>
  )
}
