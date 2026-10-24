import { useId, useState } from 'react'
import { Link } from '@tanstack/react-router'
import { useTranslation } from 'react-i18next'
import { ChevronDown, CircleHelp } from 'lucide-react'
import { ErrorState } from '@/components/ui/state'
import { Skeleton } from '@/components/ui/skeleton'
import { Tooltip } from '@/components/ui/tooltip'
import { describeError } from '@/lib/i18n'
import { formatBp, formatCount, formatMoneyAggregate } from '@/lib/money'
import { useDashboardUsage } from './data'
import type { TrendMetric } from '@/features/analytics/trend-data'
import { StatisticsStatus } from './StatisticsStatus'
import { cn } from '@/lib/utils'
import type { DateRange } from '@/components/ui/date-range'
import { dashboardSearch } from './period'

type Metric = {
  label: string
  /// 读屏与链接名里用的全称（可见标签更短时）。
  name?: string
  value: string
  /// 数值后面的小字单位（ms、Token/s）；没有数值时不显示。
  unit?: string
  measure?: TrendMetric
  hint?: string
  /// 数值下面的一行说明：样本数、分子分母。
  sub?: string
  rate?: { pct: number; tone: 'good' | 'warn' | 'bad' }
  meter?: boolean
}

/// 调用质量：一张卡，五格并排（成功率、首字延迟、平均时延、输出吞吐、缓存命中），
/// 每格是数值 + 一行样本说明，可点进对应指标的趋势；费用明细（成本、毛利、覆盖率）按需在下方展开。
export function OperationsSummary({ days, range }: { days: number; range?: DateRange | null }) {
  const { t, i18n } = useTranslation()
  const [costExpanded, setCostExpanded] = useState(false)
  const costPanelId = useId()
  const locale = i18n.language
  const query = useDashboardUsage(days, range)
  const search = dashboardSearch(days, range)
  const total = query.isError ? undefined : query.data?.total
  const known = (total?.cost_known_requests ?? 0) > 0
  const hasRequests = (total?.requests ?? 0) > 0
  const partialCost = hasRequests && (total?.cost_coverage_bp ?? 0) < 10000
  const money = (value: number | null | undefined) => value == null ? '—' : formatMoneyAggregate(value, locale)
  const milliseconds = (value: number | null | undefined, available: boolean) => available && value != null ? { value: formatCount(value, locale), unit: 'ms' } : { value: '—' }
  const throughputSamples = total?.output_tps_samples ?? 0
  const partialThroughput = hasRequests && total?.avg_output_tps_milli == null && throughputSamples > 0 && total?.observed_output_tps_milli != null
  const throughput = partialThroughput ? total?.observed_output_tps_milli : total?.avg_output_tps_milli
  const cacheSamples = total?.measured_cache_hit_requests ?? 0
  const partialCache = hasRequests && total?.cache_hit_bp == null && cacheSamples > 0 && total?.measured_cache_hit_bp != null
  const cache = partialCache ? total?.measured_cache_hit_bp : total?.cache_hit_bp
  const ttftSamples = total?.ttft_samples ?? 0
  const requests = total?.requests ?? 0
  // 样本数等于全部请求时直接写“样本 N 次”；只覆盖了一部分就写“样本 n / 总数 次”，不把样本值当成全量。
  const sampleText = (n: number) => n >= requests ? t('admin:dashboardSampleCount', { n: formatCount(n, locale) }) : t('admin:dashboardMeasuredSamples', { n: formatCount(n, locale), total: formatCount(requests, locale) })
  // 阈值与 KPI 的错误率一致（1% 提醒、5% 告警 → 成功率 99% / 95%）；成本覆盖率不足 100% 即提醒，不足一半告警。
  const grade = (pct: number | null, warn: number, bad: number) => pct == null ? undefined : { pct: Math.max(0, Math.min(100, pct)), tone: pct < bad ? 'bad' as const : pct < warn ? 'warn' as const : 'good' as const }
  const successRate = grade(hasRequests && total?.errors != null ? (1 - total.errors / requests) * 100 : null, 99, 95)
  const coverageRate = grade(hasRequests && total?.cost_coverage_bp != null ? total.cost_coverage_bp / 100 : null, 100, 50)
  const costMetrics: Metric[] = [
    { label: t('admin:dashboardKnownCost'), value: known ? money(total?.known_cost_micro) : '—' },
    { label: t('analysis:coveredMargin'), value: known ? money(total?.known_margin_micro) : '—' },
    { label: t('admin:dashboardCostCoverage'), value: hasRequests && total?.cost_coverage_bp != null ? formatBp(total.cost_coverage_bp, locale) : '—', rate: coverageRate, meter: true },
  ]
  const ttft = milliseconds(total?.avg_ttft_ms, ttftSamples > 0)
  const latency = milliseconds(total?.avg_latency_ms, hasRequests)
  const qualityMetrics: Metric[] = [
    {
      label: t('charts:metric_success'),
      value: hasRequests && total?.errors != null ? formatBp(Math.round((1 - total.errors / requests) * 10000), locale) : '—',
      measure: 'error_rate', rate: successRate,
      sub: hasRequests && total?.errors != null ? t('admin:dashboardSuccessOf', { ok: formatCount(requests - total.errors, locale), total: formatCount(requests, locale) }) : undefined,
    },
    {
      label: t('charts:ttftLabel'), name: t('analytics:ttft'), ...ttft, measure: 'ttft',
      hint: ttftSamples > 0 ? t('charts:ttftHint', { count: ttftSamples }) : t('charts:ttftUnavailable'),
      sub: ttftSamples > 0 ? sampleText(ttftSamples) : query.isPending ? undefined : t(query.isError ? 'charts:statisticsUnavailable' : 'charts:ttftNoSamples'),
    },
    { label: t('charts:metric_latency'), ...latency, measure: 'latency', sub: hasRequests ? t('admin:dashboardLatencyCaption') : undefined },
    {
      label: t('charts:throughput'),
      ...(hasRequests && throughput != null ? { value: (throughput / 1000).toLocaleString(locale, { maximumFractionDigits: 1 }), unit: 'Token/s' } : { value: '—' }),
      measure: 'throughput', hint: partialThroughput ? t('charts:throughputSampleHint') : t('charts:throughputHint'),
      // 全量口径完整时不再列样本数：那是部分采集时才有的限定
      sub: partialThroughput ? sampleText(throughputSamples) : undefined,
    },
    {
      label: t('portal:cacheHitShort'),
      value: hasRequests && cache != null && (partialCache || (total?.prompt_tokens ?? 0) > 0) ? formatBp(cache, locale) : '—',
      measure: 'cache', hint: t(partialCache ? 'charts:cacheSampleHint' : 'charts:cacheHitHint'),
      sub: partialCache ? sampleText(cacheSamples) : undefined,
    },
  ]
  const renderMetric = ({ label, name, value, unit, measure, hint, sub, rate, meter }: Metric) => {
    const shown = unit && value !== '—' ? `${value} ${unit}` : value
    // 数值与单位同属一个链接；单位小一号、不加粗
    const content = <>{value}{unit && value !== '—' && <>{' '}<span className="text-xs font-normal text-muted-foreground/80">{unit}</span></>}</>
    return <div key={label} className="group relative flex min-w-0 flex-col gap-0.5 border-r border-border/60 px-3.5 py-2.5 has-[a]:hover:bg-muted/40">
      <dt className="text-xs text-muted-foreground">{label}</dt>
      <dd className={cn('text-[17px] leading-6 font-semibold tabular-nums', rate?.tone === 'warn' && 'text-warning', rate?.tone === 'bad' && 'text-destructive')}>
        {measure ? <Tooltip content={hint ?? ''}><Link to="/admin/stats" search={{ ...search, measure }} aria-label={t('admin:dashboardOpenMetric', { label: name ?? label, value: shown })} className="outline-none transition-colors after:absolute after:inset-0 hover:text-primary focus-visible:after:rounded focus-visible:after:ring-2 focus-visible:after:ring-primary/40">
          {content}
        </Link></Tooltip> : content}
      </dd>
      {sub && <p className="text-xs text-muted-foreground/80">{sub}</p>}
      {/* 成功率只给数值上色，与门户总览一致；进度条只留给成本覆盖率 */}
      {rate && meter && <div aria-hidden className="mt-1 h-1 overflow-hidden rounded-full bg-muted"><div className={cn('h-full rounded-full', rate.tone === 'good' ? 'bg-success' : rate.tone === 'warn' ? 'bg-warning' : 'bg-destructive')} style={{ width: `${rate.pct}%` }} /></div>}
    </div>
  }
  return <section className="min-w-0 overflow-hidden rounded-xl border border-border bg-card" aria-label={t('admin:dashboardOperations')} aria-busy={query.isPending}>
    <header className="flex flex-wrap items-center gap-x-3 gap-y-0.5 border-b border-border/70 px-3.5 py-1.5">
      <h2 className="text-[15px] font-semibold">{t('admin:dashboardQuality')}</h2>
      <StatisticsStatus query={query} />
      <div className="ml-auto flex items-center gap-x-3">
        <Tooltip content={partialCost && !costExpanded ? t('admin:dashboardPartialCost') : ''}>
          <button type="button" aria-expanded={costExpanded} aria-controls={costPanelId} onClick={() => setCostExpanded((expanded) => !expanded)} className="inline-flex min-h-11 items-center gap-1.5 rounded px-1 text-xs text-muted-foreground outline-none hover:bg-muted/60 hover:text-foreground focus-visible:ring-2 focus-visible:ring-primary/40 sm:min-h-7">
            {partialCost && <span aria-hidden className="size-1.5 shrink-0 rounded-full bg-warning" />}
            {t('admin:dashboardCostDetails')}
            <ChevronDown aria-hidden size={13} className={cn('transition-transform motion-reduce:transition-none', costExpanded && 'rotate-180')} />
          </button>
        </Tooltip>
        <Link to="/admin/stats" search={{ ...search, measure: 'latency' }} aria-label={t('admin:dashboardQualityDetails')} className="inline-flex min-h-11 items-center rounded text-[13px] text-primary outline-none hover:underline focus-visible:ring-2 focus-visible:ring-primary/40 sm:min-h-7">{t('admin:dashboardQualityTrend')}<span aria-hidden>&nbsp;→</span></Link>
      </div>
    </header>
    {query.isError && <div className="p-3"><ErrorState message={describeError(query.error)} onRetry={() => void query.refetch()} /></div>}
    {query.isPending ? <div aria-hidden className="grid grid-cols-[repeat(auto-fit,minmax(10.625rem,1fr))]">
      {qualityMetrics.map((metric) => <div key={metric.label} className="flex flex-col gap-1.5 px-3.5 py-3"><Skeleton className="h-3 w-14" /><Skeleton className="h-5 w-20" /><Skeleton className="h-3 w-24" /></div>)}
    </div> : <dl className="-mr-px grid grid-cols-[repeat(auto-fit,minmax(10.625rem,1fr))]">
      {qualityMetrics.map(renderMetric)}
    </dl>}
    <div id={costPanelId} hidden={!costExpanded}>
      {costExpanded && <div className="border-t border-border/70">
        <dl className="-mr-px grid grid-cols-[repeat(auto-fit,minmax(10.625rem,1fr))]">
          {costMetrics.map(renderMetric)}
        </dl>
        {partialCost && <Tooltip content={t('admin:dashboardPartialCost')}>
          <button type="button" className="mx-3 mb-2 inline-flex min-h-11 items-center gap-1 rounded px-1 text-xs text-warning outline-none focus-visible:ring-2 focus-visible:ring-primary/40 sm:min-h-7">
            <CircleHelp aria-hidden size={13} />{t('admin:dashboardPartialCostShort')}
          </button>
        </Tooltip>}
      </div>}
    </div>
  </section>
}
