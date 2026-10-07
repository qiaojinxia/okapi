import { useId, useState } from 'react'
import { Link } from '@tanstack/react-router'
import { useTranslation } from 'react-i18next'
import { ChevronDown, CircleHelp } from 'lucide-react'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { ErrorState, LoadingState } from '@/components/ui/state'
import { Tooltip } from '@/components/ui/tooltip'
import { describeError } from '@/lib/i18n'
import { formatBp, formatCount, formatMoneyAggregate } from '@/lib/money'
import { useDashboardUsage } from './data'
import type { TrendMetric } from '@/features/analytics/trend-data'
import { StatisticsStatus } from './StatisticsStatus'
import { cn } from '@/lib/utils'
import type { DateRange } from '@/components/ui/date-range'
import { dashboardPeriodLabel, dashboardSearch } from './period'

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
  const latency = (value: number | null | undefined, available: boolean) => available && value != null ? `${formatCount(value, locale)} ms` : '—'
  const throughputSamples = total?.output_tps_samples ?? 0
  const partialThroughput = hasRequests && total?.avg_output_tps_milli == null && throughputSamples > 0 && total?.observed_output_tps_milli != null
  const throughput = partialThroughput ? total?.observed_output_tps_milli : total?.avg_output_tps_milli
  const cacheSamples = total?.measured_cache_hit_requests ?? 0
  const partialCache = hasRequests && total?.cache_hit_bp == null && cacheSamples > 0 && total?.measured_cache_hit_bp != null
  const cache = partialCache ? total?.measured_cache_hit_bp : total?.cache_hit_bp
  const sampleText = (n: number) => t('admin:dashboardMeasuredSamples', { n: formatCount(n, locale), total: formatCount(total?.requests ?? 0, locale) })
  // 阈值与 KPI 的错误率一致（1% 提醒、5% 告警 → 成功率 99% / 95%）；成本覆盖率不足 100% 即提醒，不足一半告警。
  const grade = (pct: number | null, warn: number, bad: number) => pct == null ? undefined : { pct: Math.max(0, Math.min(100, pct)), tone: pct < bad ? 'bad' as const : pct < warn ? 'warn' as const : 'good' as const }
  const successRate = grade(hasRequests && total?.errors != null ? (1 - total.errors / total.requests!) * 100 : null, 99, 95)
  const coverageRate = grade(hasRequests && total?.cost_coverage_bp != null ? total.cost_coverage_bp / 100 : null, 100, 50)
  type Metric = { label: string; value: string; measure?: TrendMetric; hint?: string; sample?: string; rate?: ReturnType<typeof grade>; meter?: boolean }
  const costMetrics: Metric[] = [
    { label: t('admin:dashboardKnownCost'), value: known ? money(total?.known_cost_micro) : '—' },
    { label: t('analysis:coveredMargin'), value: known ? money(total?.known_margin_micro) : '—' },
    { label: t('admin:dashboardCostCoverage'), value: hasRequests && total?.cost_coverage_bp != null ? formatBp(total.cost_coverage_bp, locale) : '—', rate: coverageRate, meter: true },
  ]
  const qualityMetrics: Metric[] = [
    { label: t('charts:metric_success'), value: hasRequests && total?.errors != null ? formatBp(Math.round((1 - total.errors / total.requests!) * 10000), locale) : '—', measure: 'error_rate', rate: successRate },
    { label: t('charts:metric_latency'), value: latency(total?.avg_latency_ms, hasRequests), measure: 'latency' },
    { label: t('analytics:ttft'), value: latency(total?.avg_ttft_ms, (total?.ttft_samples ?? 0) > 0), measure: 'ttft', hint: (total?.ttft_samples ?? 0) > 0 ? t('charts:ttftHint', { count: total?.ttft_samples }) : t('charts:ttftUnavailable') },
    { label: t('charts:throughput'), value: hasRequests && throughput != null ? `${(throughput / 1000).toLocaleString(locale, { maximumFractionDigits: 1 })} Token/s` : '—', measure: 'throughput', hint: partialThroughput ? t('charts:throughputSampleHint') : t('charts:throughputHint'), sample: partialThroughput ? sampleText(throughputSamples) : undefined },
    { label: t('portal:cacheHitShort'), value: hasRequests && cache != null && (partialCache || (total?.prompt_tokens ?? 0) > 0) ? formatBp(cache, locale) : '—', measure: 'cache', hint: t(partialCache ? 'charts:cacheSampleHint' : 'charts:cacheHitHint'), sample: partialCache ? sampleText(cacheSamples) : undefined },
  ]
  const renderMetric = ({ label, value, measure, hint, sample, rate, meter }: Metric) => <div key={label} className="group relative min-h-11 min-w-0 rounded px-1 py-0.5 has-[a]:hover:bg-muted/60 sm:min-h-10 sm:py-0">
    <dt className={cn('text-xs text-muted-foreground', measure === 'ttft' && 'font-medium text-primary')}>{measure === 'ttft' ? t('charts:ttftLabel') : label}</dt>
    <dd className={cn('mt-0.5 break-words text-base font-semibold tabular-nums', rate?.tone === 'warn' && 'text-warning', rate?.tone === 'bad' && 'text-destructive')}>
      {measure ? <Tooltip content={hint ?? ''}><Link to="/admin/stats" search={{ ...search, measure }} aria-label={t('admin:dashboardOpenMetric', { label, value })} className="underline-offset-4 outline-none after:absolute after:inset-0 after:rounded hover:underline focus-visible:after:ring-2 focus-visible:after:ring-primary/40">
        {value}
      </Link></Tooltip> : value}
    </dd>
    {sample && <p className="text-[11px] text-muted-foreground">{sample}</p>}
    {/* 成功率只给数值上色，与门户总览一致；进度条只留给成本覆盖率 */}
    {rate && meter && <div aria-hidden className="mt-1.5 h-1 overflow-hidden rounded-full bg-muted"><div className={cn('h-full rounded-full', rate.tone === 'good' ? 'bg-success' : rate.tone === 'warn' ? 'bg-warning' : 'bg-destructive')} style={{ width: `${rate.pct}%` }} /></div>}
    {measure === 'ttft' && (total?.ttft_samples ?? 0) === 0 && !query.isPending && <p className="text-[11px] text-muted-foreground">{t(query.isError ? 'charts:statisticsUnavailable' : 'charts:ttftNoSamples')}</p>}
  </div>
  return <Card className="min-w-0 rounded-xl" role="region" aria-label={t('admin:dashboardOperations')}>
    <CardHeader className="flex-row flex-wrap items-center justify-between gap-x-3 gap-y-0 px-4 pt-1 pb-0">
      <div className="flex min-w-0 flex-wrap items-center gap-x-3">
        <CardTitle>{t('admin:dashboardOperations')} <span className="ml-2 font-normal text-muted-foreground">{dashboardPeriodLabel(days, range, t)}</span></CardTitle>
      </div>
      <div className="flex flex-wrap items-center gap-x-3">
        <StatisticsStatus query={query} />
        <Link to="/admin/stats" search={{ ...search, measure: 'latency' }} className="inline-flex min-h-11 items-center rounded text-xs text-primary outline-none underline-offset-4 hover:underline focus-visible:ring-2 focus-visible:ring-primary/40 sm:min-h-7">{t('admin:dashboardQualityDetails')}</Link>
        <Tooltip content={partialCost && !costExpanded ? t('admin:dashboardPartialCost') : ''}>
          <button type="button" aria-expanded={costExpanded} aria-controls={costPanelId} onClick={() => setCostExpanded((expanded) => !expanded)} className="inline-flex min-h-11 items-center gap-1.5 rounded px-1 text-xs text-muted-foreground outline-none hover:bg-muted/60 hover:text-foreground focus-visible:ring-2 focus-visible:ring-primary/40 sm:min-h-7">
            {partialCost && <span aria-hidden className="size-1.5 shrink-0 rounded-full bg-warning" />}
            {t('admin:dashboardCostDetails')}
            <ChevronDown aria-hidden size={13} className={cn('transition-transform motion-reduce:transition-none', costExpanded && 'rotate-180')} />
          </button>
        </Tooltip>
      </div>
    </CardHeader>
    <CardContent className="px-3 pt-1 pb-2 sm:pb-1">
      {query.isError && <ErrorState message={describeError(query.error)} onRetry={() => void query.refetch()} />}
      {query.isPending && <LoadingState className="py-2" />}
      <dl className="grid min-w-0 grid-cols-2 gap-x-4 gap-y-2 sm:grid-cols-3 lg:grid-cols-5">
        {qualityMetrics.map(renderMetric)}
      </dl>
      <div id={costPanelId} hidden={!costExpanded}>
        {costExpanded && <div className="mt-2 border-t border-border/60 pt-2">
          <dl className="grid min-w-0 grid-cols-1 gap-x-4 gap-y-2 sm:grid-cols-3">
            {costMetrics.map(renderMetric)}
          </dl>
          {partialCost && <Tooltip content={t('admin:dashboardPartialCost')}>
            <button type="button" className="mt-1 inline-flex min-h-11 items-center gap-1 rounded px-1 text-xs text-warning outline-none focus-visible:ring-2 focus-visible:ring-primary/40 sm:min-h-7">
              <CircleHelp aria-hidden size={13} />{t('admin:dashboardPartialCostShort')}
            </button>
          </Tooltip>}
        </div>}
      </div>
    </CardContent>
  </Card>
}
