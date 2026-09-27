import { Link } from '@tanstack/react-router'
import { useTranslation } from 'react-i18next'
import { CircleHelp } from 'lucide-react'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { ErrorState, LoadingState } from '@/components/ui/state'
import { Tooltip } from '@/components/ui/tooltip'
import { describeError } from '@/lib/i18n'
import { formatBp, formatCount, formatMoneyAggregate } from '@/lib/money'
import { useDashboardUsage } from './data'
import type { TrendMetric } from '@/features/analytics/trend-data'
import { StatisticsStatus } from './StatisticsStatus'
import { cn } from '@/lib/utils'

export function OperationsSummary({ days }: { days: number }) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const query = useDashboardUsage(days)
  const total = query.isError ? undefined : query.data?.total
  const known = (total?.cost_known_requests ?? 0) > 0
  const hasRequests = (total?.requests ?? 0) > 0
  const money = (value: number | null | undefined) => value == null ? '—' : formatMoneyAggregate(value, locale)
  const latency = (value: number | undefined, available: boolean) => available && value != null ? `${formatCount(value, locale)} ms` : '—'
  const metrics: { label: string; value: string; measure?: TrendMetric; hint?: string }[] = [
    { label: t('admin:dashboardKnownCost'), value: known ? money(total?.known_cost_micro) : '—' },
    { label: t('analysis:coveredMargin'), value: known ? money(total?.known_margin_micro) : '—' },
    { label: t('admin:dashboardCostCoverage'), value: hasRequests && total?.cost_coverage_bp != null ? formatBp(total.cost_coverage_bp, locale) : '—' },
    { label: t('charts:metric_success'), value: hasRequests && total?.errors != null ? formatBp(Math.round((1 - total.errors / total.requests!) * 10000), locale) : '—', measure: 'error_rate' },
    { label: t('charts:metric_latency'), value: latency(total?.avg_latency_ms, hasRequests), measure: 'latency' },
    { label: t('analytics:ttft'), value: latency(total?.avg_ttft_ms, (total?.ttft_samples ?? 0) > 0), measure: 'ttft', hint: (total?.ttft_samples ?? 0) > 0 ? t('charts:ttftHint', { count: total?.ttft_samples }) : t('charts:ttftUnavailable') },
    { label: t('charts:throughput'), value: hasRequests && total?.avg_output_tps_milli != null ? `${(total.avg_output_tps_milli / 1000).toLocaleString(locale, { maximumFractionDigits: 1 })} Token/s` : '—', measure: 'throughput', hint: t('charts:throughputHint') },
    { label: t('portal:cacheHitShort'), value: (total?.prompt_tokens ?? 0) > 0 && total?.cache_hit_bp != null ? formatBp(total.cache_hit_bp, locale) : '—', measure: 'cache' },
  ]
  return <Card className="min-w-0 rounded-xl" role="region" aria-label={t('admin:dashboardOperations')}>
    <CardHeader className="flex-row flex-wrap items-center justify-between gap-x-3 gap-y-0 px-4 pt-1 pb-0">
      <div className="flex min-w-0 flex-wrap items-center gap-x-3">
        <CardTitle>{t('admin:dashboardOperations')} <span className="ml-2 font-normal text-muted-foreground">{t('admin:lastDays', { days })}</span></CardTitle>
        {hasRequests && (total?.cost_coverage_bp ?? 0) < 10000 && <Tooltip content={t('admin:dashboardPartialCost')}>
          <button type="button" className="inline-flex min-h-11 items-center gap-1 rounded text-xs text-warning outline-none focus-visible:ring-2 focus-visible:ring-primary/40 sm:min-h-7">
            <CircleHelp aria-hidden size={13} />{t('admin:dashboardPartialCostShort')}
          </button>
        </Tooltip>}
      </div>
      <div className="flex flex-wrap items-center gap-x-3">
        <StatisticsStatus query={query} />
        <Link to="/admin/stats" search={{ days, measure: 'latency' }} className="inline-flex min-h-11 items-center rounded text-xs text-primary outline-none underline-offset-4 hover:underline focus-visible:ring-2 focus-visible:ring-primary/40 sm:min-h-7">{t('admin:dashboardQualityDetails')}</Link>
      </div>
    </CardHeader>
    <CardContent className="px-3 pt-1 pb-2 sm:pb-1">
      {query.isError && <ErrorState message={describeError(query.error)} onRetry={() => void query.refetch()} />}
      {query.isPending && <LoadingState className="py-2" />}
        <dl className="grid min-w-0 grid-cols-2 gap-x-2 gap-y-2 md:grid-cols-4 lg:grid-cols-8">
          {metrics.map(({ label, value, measure, hint }, index) => <div key={label} className={cn('group relative min-h-11 min-w-0 rounded px-1 py-0.5 has-[a]:hover:bg-muted/60 sm:min-h-10 sm:py-0', index === 2 && 'lg:border-r lg:border-border lg:pr-2', measure === 'ttft' && 'bg-primary/10 ring-1 ring-primary/20')}>
            <dt className={cn('text-xs text-muted-foreground', measure === 'ttft' && 'font-semibold text-primary')}>{measure === 'ttft' ? t('charts:ttftLabel') : label}</dt>
            <dd className="mt-0.5 break-words text-base font-semibold tabular-nums">
              {measure ? <Tooltip content={hint ?? ''}><Link to="/admin/stats" search={{ days, measure }} aria-label={t('admin:dashboardOpenMetric', { label, value })} className={cn('text-sm underline-offset-4 outline-none after:absolute after:inset-0 after:rounded hover:underline focus-visible:after:ring-2 focus-visible:after:ring-primary/40', measure === 'ttft' && 'text-base')}>
                {value}
              </Link></Tooltip> : value}
            </dd>
            {measure === 'ttft' && (total?.ttft_samples ?? 0) === 0 && !query.isPending && <p className="text-[11px] text-muted-foreground">{t(query.isError ? 'charts:statisticsUnavailable' : 'charts:ttftNoSamples')}</p>}
          </div>)}
        </dl>
    </CardContent>
  </Card>
}
