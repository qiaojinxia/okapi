import { Activity, AlertTriangle, Coins, Cpu, Database, Timer } from 'lucide-react'
import type { LucideIcon } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { Card } from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import type { TrendMetric } from '@/features/analytics/trend-data'
import type { CubeMetrics } from '@/lib/cube'
import { formatBp, formatCount, formatMoneyAggregate } from '@/lib/money'
import { cn } from '@/lib/utils'

const DELTA_PILL = 'inline-flex shrink-0 items-center rounded-full px-1.5 py-px text-[11px] leading-4 font-medium tabular-nums'

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
  prev: number | null | undefined
  kind: 'count' | 'bp'
  invert?: boolean
  locale: string
}) {
  const { t } = useTranslation()
  if (prev == null) return null
  let text: string
  let up: boolean
  if (kind === 'bp') {
    const pp = (cur - prev) / 100
    if (Math.abs(pp) < 0.05) {
      return <span className={cn(DELTA_PILL, 'bg-muted/85 text-muted-foreground')}>{t('analytics:deltaFlat')}</span>
    }
    up = pp > 0
    text = `${up ? '+' : ''}${pp.toLocaleString(locale, { maximumFractionDigits: 1 })}pp`
  } else {
    if (prev === 0) {
      return cur > 0 ? <span className={cn(DELTA_PILL, 'bg-muted/85 text-muted-foreground')}>{t('analytics:deltaNew')}</span> : null
    }
    const pct = ((cur - prev) / Math.abs(prev)) * 100
    if (Math.abs(pct) < 0.5) {
      return <span className={cn(DELTA_PILL, 'bg-muted/85 text-muted-foreground')}>{t('analytics:deltaFlat')}</span>
    }
    up = pct > 0
    text = `${up ? '+' : ''}${pct.toLocaleString(locale, { maximumFractionDigits: 0 })}%`
  }
  const good = invert ? !up : up
  return (
    <span
      className={cn(
        DELTA_PILL,
        good
          ? 'bg-success/12 text-success ring-1 ring-inset ring-success/20'
          : 'bg-destructive/10 text-destructive ring-1 ring-inset ring-destructive/20',
      )}
    >
      {up ? '▲' : '▼'} {text}
    </span>
  )
}

/// KPI 里的金额：六列仪表带每格约 190px，万元以上转紧凑记法（$71.5K），精确值在拆分表与 tooltip 里。
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
  return formatMoneyAggregate(micro, locale, true)
}

function MetricCell({
  icon: Icon,
  label,
  value,
  sub,
  tone = 'default',
  active = false,
  loading = false,
  onClick,
}: {
  icon: LucideIcon
  label: string
  value: React.ReactNode
  sub?: React.ReactNode
  tone?: 'default' | 'warn' | 'bad'
  active?: boolean
  loading?: boolean
  onClick?: () => void
}) {
  if (loading) {
    return (
      <div className="flex min-w-0 flex-col bg-card px-4 py-3.5">
        <div className="flex items-center justify-between gap-2">
          <Skeleton className="h-3.5 w-14" />
          <Skeleton className="h-3.5 w-3.5 rounded-full" />
        </div>
        <Skeleton className="mt-2 h-7 w-24" />
        <Skeleton className="mt-2.5 h-3.5 w-28" />
      </div>
    )
  }

  const Comp = onClick ? 'button' : 'div'
  return (
    <Comp
      type={onClick ? 'button' : undefined}
      aria-pressed={onClick ? active : undefined}
      onClick={onClick}
      className={cn(
        'group/kpi relative flex min-w-0 flex-col bg-card px-4 py-3.5 text-left transition-colors outline-none',
        onClick && 'cursor-pointer hover:bg-muted/35 focus-visible:z-10 focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-primary/50',
        active && 'bg-primary/[0.03] before:pointer-events-none before:absolute before:inset-x-0 before:top-0 before:h-[2px] before:bg-primary dark:bg-primary/[0.07]',
      )}
    >
      <Icon
        aria-hidden
        className={cn(
          'pointer-events-none absolute top-3.5 right-4 h-3.5 w-3.5 transition-colors',
          tone === 'bad'
            ? 'text-destructive/80'
            : tone === 'warn'
              ? 'text-warning'
              : active
                ? 'text-primary'
                : 'text-muted-foreground/45 group-hover/kpi:text-muted-foreground',
        )}
      />
      {/* label 保持作为单元容器的直接子节点，确保语义结构和测试定位稳定 */}
      <span
        className={cn(
          'pr-5 text-xs leading-4 font-medium transition-colors',
          active ? 'text-primary' : 'text-muted-foreground group-hover/kpi:text-foreground/85',
        )}
      >
        {label}
      </span>
      <span
        title={typeof value === 'string' ? value : undefined}
        className={cn(
          'mt-1 truncate text-[22px] leading-7 font-semibold tracking-tight tabular-nums',
          tone === 'bad' ? 'text-destructive' : tone === 'warn' ? 'text-warning' : 'text-foreground',
        )}
      >
        {value}
      </span>
      {sub !== undefined && (
        <span className="mt-2 flex min-w-0 w-full flex-col gap-1 text-xs leading-4 text-muted-foreground">
          {sub}
        </span>
      )}
    </Comp>
  )
}

/// 过滤后的六项 KPI 仪表带（请求 / 消费 / Tokens / 错误率 / 缓存命中 / 平均时延）。
///
/// 桌面端以一整条 6 格分隔式仪表带呈现（而非 6 张零散悬浮卡）：
/// 1. 省去卡片间缝隙后每格增宽约 25px，毛利与缓存明细不再被中途截断；
/// 2. 顶部 2px 色条 + 极浅底色直接传达「可点击切换下方趋势/排序指标」的标签栏心智。
export function KpiStrip({
  total,
  previous,
  days,
  loading,
  activeMetric,
  onPickMetric,
}: {
  total: Partial<CubeMetrics> | undefined
  previous: Partial<CubeMetrics> | undefined
  days: number
  loading: boolean
  activeMetric?: TrendMetric
  onPickMetric?: (metric: TrendMetric) => void
}) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language

  if (!total && !loading) {
    const emptyItems: { icon: LucideIcon; label: string }[] = [
      { icon: Activity, label: t('common:requests') },
      { icon: Coins, label: t('analytics:kpiSpend') },
      { icon: Cpu, label: t('common:tokens') },
      { icon: AlertTriangle, label: t('admin:errorRate') },
      { icon: Database, label: t('analytics:kpiCacheHit') },
      { icon: Timer, label: t('analytics:kpiLatency') },
    ]
    return (
      <Card data-slot="analytics-kpi-strip" className="overflow-hidden">
        <div className="grid grid-cols-2 gap-px bg-border/65 md:grid-cols-3 xl:grid-cols-6">
          {emptyItems.map((item) => (
            <MetricCell key={item.label} icon={item.icon} label={item.label} value="—" />
          ))}
        </div>
      </Card>
    )
  }

  const cur = total ?? {}
  const prev = previous ?? {}
  const vsPrev = <span className="truncate text-[11px] leading-4 text-muted-foreground/85">{t('analytics:vsPrevious', { days })}</span>
  const errBp = cur.error_rate_bp ?? 0
  const pick = (metric: TrendMetric) => (onPickMetric ? () => onPickMetric(metric) : undefined)

  const spendMeta = cur.known_margin_micro != null
    ? `${t('analysis:coveredMargin')} ${formatMoneyAggregate(cur.known_margin_micro, locale, true)} · ${t('analysis:coverage', { v: cur.cost_coverage_bp == null ? '—' : formatBp(cur.cost_coverage_bp, locale) })}`
    : (cur.discount_micro ?? 0) > 0
      ? t('analytics:kpiSaved', { v: formatMoneyAggregate(cur.discount_micro ?? 0, locale, true) })
      : null
  const tokenMixText = t('analytics:kpiTokenMix', {
    i: formatCount(cur.prompt_tokens ?? 0, locale),
    o: formatCount(cur.completion_tokens ?? 0, locale),
  })
  const cachedText = t('analytics:kpiCached', { n: formatCount(cur.cached_tokens ?? 0, locale) })

  return (
    <Card data-slot="analytics-kpi-strip" className="overflow-hidden">
      <div className="grid grid-cols-2 gap-px bg-border/65 md:grid-cols-3 xl:grid-cols-6">
        <MetricCell
          icon={Activity}
          loading={loading}
          active={activeMetric === 'requests'}
          onClick={pick('requests')}
          label={t('common:requests')}
          value={formatCount(cur.requests ?? 0, locale)}
          sub={
            <span className="flex min-w-0 items-center gap-1.5">
              <Delta cur={cur.requests ?? 0} prev={prev.requests} kind="count" locale={locale} />
              {vsPrev}
            </span>
          }
        />
        <MetricCell
          icon={Coins}
          loading={loading}
          active={activeMetric === 'amount'}
          onClick={pick('amount')}
          label={t('analytics:kpiSpend')}
          value={kpiMoney(cur.amount_micro ?? 0, locale)}
          sub={
            <>
              <span className="flex min-w-0 items-center gap-1.5">
                <Delta cur={cur.amount_micro ?? 0} prev={prev.amount_micro} kind="count" locale={locale} />
                {spendMeta === null && vsPrev}
              </span>
              {spendMeta !== null && (
                <span
                  title={cur.known_margin_micro != null ? `${spendMeta} — ${t('analysis:costHint')}` : spendMeta}
                  className={cn(
                    'block truncate text-[11px] leading-4 text-muted-foreground/85 tabular-nums',
                    cur.known_margin_micro != null && cur.known_margin_micro < 0 && 'text-destructive',
                  )}
                >
                  {spendMeta}
                </span>
              )}
            </>
          }
        />
        <MetricCell
          icon={Cpu}
          loading={loading}
          active={activeMetric === 'tokens'}
          onClick={pick('tokens')}
          label={t('common:tokens')}
          value={formatCount(cur.tokens ?? 0, locale)}
          sub={
            <>
              <span className="flex min-w-0 items-center gap-1.5">
                <Delta cur={cur.tokens ?? 0} prev={prev.tokens} kind="count" locale={locale} />
              </span>
              <span title={tokenMixText} className="block truncate text-[11px] leading-4 text-muted-foreground/85 tabular-nums">
                {tokenMixText}
              </span>
            </>
          }
        />
        <MetricCell
          icon={AlertTriangle}
          loading={loading}
          active={activeMetric === 'error_rate'}
          onClick={pick('error_rate')}
          label={t('admin:errorRate')}
          value={(cur.requests ?? 0) > 0 ? formatBp(errBp, locale) : '—'}
          tone={errBp >= 500 ? 'bad' : errBp >= 100 ? 'warn' : 'default'}
          sub={
            <span className="flex min-w-0 items-center gap-1.5">
              {(cur.requests ?? 0) > 0 && (prev.requests ?? 0) > 0 && <Delta cur={errBp} prev={prev.error_rate_bp} kind="bp" invert locale={locale} />}
              <span className="truncate text-[11px] leading-4 text-muted-foreground/85 tabular-nums">
                {t('analytics:kpiErrors', { n: formatCount(cur.errors ?? 0, locale) })}
              </span>
            </span>
          }
        />
        <MetricCell
          icon={Database}
          loading={loading}
          active={activeMetric === 'cache'}
          onClick={pick('cache')}
          label={t('analytics:kpiCacheHit')}
          value={(cur.prompt_tokens ?? 0) > 0 && cur.cache_hit_bp != null ? formatBp(cur.cache_hit_bp, locale) : '—'}
          sub={
            <>
              {cur.cache_hit_bp != null && prev.cache_hit_bp != null && (
                <span className="flex min-w-0 items-center gap-1.5">
                  <Delta cur={cur.cache_hit_bp} prev={prev.cache_hit_bp} kind="bp" locale={locale} />
                </span>
              )}
              <span title={cachedText} className="block truncate text-[11px] leading-4 text-muted-foreground/85 tabular-nums">
                {cachedText}
              </span>
            </>
          }
        />
        <MetricCell
          icon={Timer}
          loading={loading}
          active={activeMetric === 'latency'}
          onClick={pick('latency')}
          label={t('analytics:kpiLatency')}
          value={(cur.requests ?? 0) > 0 && cur.avg_latency_ms != null ? `${formatCount(cur.avg_latency_ms, locale)} ms` : '—'}
          sub={
            <span className="flex min-w-0 items-center gap-1.5">
              {(cur.requests ?? 0) > 0 && (prev.requests ?? 0) > 0 && cur.avg_latency_ms != null && (
                <Delta
                  cur={cur.avg_latency_ms}
                  prev={prev.avg_latency_ms}
                  kind="count"
                  invert
                  locale={locale}
                />
              )}
              {cur.avg_ttft_ms != null && (cur.ttft_samples ?? cur.avg_ttft_ms) > 0 ? (
                <span className="truncate text-[11px] leading-4 text-muted-foreground/85 tabular-nums">
                  {t('analytics:kpiTtft', { v: formatCount(cur.avg_ttft_ms, locale) })}
                </span>
              ) : (
                vsPrev
              )}
            </span>
          }
        />
      </div>
    </Card>
  )
}
