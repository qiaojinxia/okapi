import { Link } from '@tanstack/react-router'
import { CircleHelp, ArrowUpRight, FlaskConical } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { ErrorState, LoadingState } from '@/components/ui/state'
import { Tooltip } from '@/components/ui/tooltip'
import type { CubeMetrics, TokenSource } from '@/features/analytics/types'
import { segments } from '@/features/portal-overview/TokenMixView'
import { describeError } from '@/lib/i18n'
import { formatBp, formatCount } from '@/lib/money'
import { useDashboardUsage } from './data'

const sources: TokenSource[] = ['upstream', 'estimated', 'local_override', 'unknown']
const sourceColors = { upstream: 'bg-success', estimated: 'bg-warning', local_override: 'bg-primary', unknown: 'bg-muted-foreground' }

function SourceQuality({ total }: { total: Partial<CubeMetrics> }) {
  const { t, i18n } = useTranslation()
  const count = (n: number | null | undefined) => n == null ? '—' : formatCount(n, i18n.language)
  const sourceTotal = (source: TokenSource) => {
    const input = total.token_provenance?.prompt[source].tokens, output = total.token_provenance?.completion[source].tokens
    return input == null || output == null ? null : input + output
  }
  return <details className="group border-t border-border pt-1.5">
    <summary className="flex cursor-pointer list-none flex-wrap items-center justify-between gap-x-2 gap-y-1 rounded text-xs outline-none focus-visible:ring-2 focus-visible:ring-primary/40">
      <span className="inline-flex items-center gap-1 font-medium"><CircleHelp aria-hidden size={12} />{t('logs:usageSource')}<span aria-hidden className="text-muted-foreground group-open:rotate-90">›</span></span>
      <span className="text-muted-foreground">{t('admin:dashboardSourcePreview', {
        upstream: count(sourceTotal('upstream')),
        unknown: count(sourceTotal('unknown')),
      })}</span>
    </summary>
    {!total.token_provenance ? <p className="mt-2 text-xs text-muted-foreground">{t('admin:dashboardSourceUnavailable')}</p> : <>
      <div role="group" className="mt-2 grid grid-cols-2 gap-2 lg:grid-cols-4" aria-label={t('logs:usageSource')}>
        {sources.map((source) => {
          const input = total.token_provenance!.prompt[source], output = total.token_provenance!.completion[source]
          const sum = input.tokens == null || output.tokens == null ? null : input.tokens + output.tokens
          return <div key={source} className="min-w-0 rounded-md bg-muted/40 px-2 py-1.5 text-xs">
            <p className="flex items-center gap-1 text-muted-foreground"><span aria-hidden className={`h-1.5 w-1.5 shrink-0 rounded-full ${sourceColors[source]}`} />{t(`logs:source_${source}`)}</p>
            <p className="mt-0.5 font-medium tabular-nums" title={sum?.toLocaleString(i18n.language)}>{count(sum)}</p>
            <p className="mt-0.5 text-[11px] text-muted-foreground">{t('admin:dashboardSourceAxes', { input: count(input.tokens), output: count(output.tokens) })}</p>
          </div>
        })}
      </div>
      <p className="mt-1.5 text-[11px] leading-4 text-muted-foreground">{t('admin:dashboardSourceHint')}</p>
    </>}
  </details>
}

export function TokenSummary({ days }: { days: number }) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language, query = useDashboardUsage(days)
  const total = query.isError ? undefined : query.data?.total
  const collected = total?.prompt_tokens != null && total.completion_tokens != null
  const requests = total?.requests ?? 0
  const readCount = total?.cache_read_known_requests
  const writeCount = total?.cache_write_known_requests
  const cached = total?.cached_tokens ?? 0
  const writes = total?.recorded_cache_write_tokens ?? total?.cache_write_tokens
  const reasoning = total?.reasoning_tokens ?? 0
  const parts = collected ? segments({ prompt_tokens: total!.prompt_tokens!, completion_tokens: total!.completion_tokens!, cached_tokens: cached, cache_write_tokens: writes, reasoning_tokens: reasoning }) : []
  const sum = collected ? total!.prompt_tokens! + total!.completion_tokens! : 0
  const labels = { input: t('portal:tokInput'), cached: t('portal:tokCached'), write: t('charts:cacheWrite'), output: t('portal:tokOutput'), reasoning: t('portal:tokReasoning') }
  const count = (n: number | null | undefined) => n == null ? '—' : formatCount(n, locale)
  const coverage = (n: number | undefined) => n == null || requests <= 0 ? '—' : `${count(n)} / ${count(requests)} · ${formatBp(Math.floor(n * 10000 / requests), locale)}`
  const readComplete = requests > 0 && readCount === requests
  const writeComplete = requests > 0 && writeCount === requests
  const partial = !readComplete || !writeComplete
  return <Card role="region" aria-label={t('portal:tokenSnapshot')} className="min-w-0 rounded-xl sm:col-span-2">
    <CardHeader className="gap-0 px-3 pt-2 pb-1 lg:pt-1">
      <div className="flex flex-wrap items-center justify-between gap-x-2">
        <CardTitle className="flex flex-wrap items-baseline gap-x-2">
          <span>{t('portal:tokenSnapshot')}</span>
          {query.isSuccess && collected && <span className="font-semibold tabular-nums" title={sum.toLocaleString(locale)}>{count(sum)} <span className="text-xs font-normal text-muted-foreground">{t('common:tokens')}</span></span>}
          <span className="text-xs font-normal text-muted-foreground">{t('admin:lastDays', { days })}</span>
        </CardTitle>
        <Link to="/admin/stats" search={{ days, measure: 'tokens' }} className="inline-flex min-h-8 shrink-0 items-center gap-1 rounded text-xs text-primary outline-none hover:underline focus-visible:ring-2 focus-visible:ring-primary/40 lg:min-h-6">{t('portal:expandTrend')}<ArrowUpRight aria-hidden size={13} /></Link>
      </div>
    </CardHeader>
    <CardContent className="space-y-2 px-3 pt-0 pb-2">
      {query.isError ? <ErrorState message={describeError(query.error)} onRetry={() => void query.refetch()} /> : query.isPending ? <LoadingState /> : !collected ? <p className="py-6 text-xs text-muted-foreground">{t('analysis:notCollected')}</p> : <>
        <div className="flex flex-wrap items-center gap-x-2 gap-y-1 text-[11px]">
          <Tooltip content={t('admin:dashboardRecordedTokenHint')}><Badge variant="muted" tabIndex={0} className="min-h-5 text-[11px]">{t('admin:dashboardRecordedTokens')}</Badge></Tooltip>
          <span className="text-muted-foreground">{t('admin:dashboardTokenTotal', { input: count(total?.prompt_tokens), output: count(total?.completion_tokens) })}</span>
        </div>
        <Tooltip className="block" content={t('admin:dashboardTokenHint')}>
          <div tabIndex={0} role="img" aria-label={t('admin:dashboardTokenHint')} className="flex h-2.5 overflow-hidden rounded-full bg-muted outline-none focus-visible:ring-2 focus-visible:ring-primary/40">
            {parts.filter((part) => part.value > 0).map((part) => <div aria-hidden key={part.key} className={part.className} style={{ width: `${sum > 0 ? part.value / sum * 100 : 0}%` }} />)}
          </div>
        </Tooltip>
        <dl data-slot="token-segments" className="grid grid-cols-2 gap-x-3 gap-y-1.5 min-[480px]:grid-cols-5">
          {parts.map((part) => {
            const known = part.key === 'cached' ? cached > 0 || (readCount ?? 0) > 0 : part.key === 'write' ? writes != null : part.key === 'reasoning' ? reasoning > 0 || (total?.token_detail_observations?.reasoning_tokens?.observed_records ?? 0) > 0 : true
            const incomplete = part.key === 'cached' ? !readComplete : part.key === 'write' ? !writeComplete : false
            return <div key={part.key} className="min-w-0 text-xs">
              <dt className="flex items-center gap-1 text-muted-foreground"><span aria-hidden className={`h-2 w-2 shrink-0 rounded-sm ${part.className}`} />{labels[part.key]}</dt>
              <dd className="mt-0.5 font-medium tabular-nums" title={known ? part.value.toLocaleString(locale) : undefined}>{known ? count(part.value) : '—'}{known && incomplete && <span className="ml-1 text-[10px] font-normal text-warning">{t('admin:dashboardPartialTokens')}</span>}</dd>
            </div>
          })}
        </dl>
        <div role="group" className="grid gap-x-3 gap-y-1 rounded-md bg-muted/40 px-2 py-1.5 text-[11px] sm:grid-cols-2" aria-label={t('admin:dashboardCacheCoverage')}>
          <p className="flex flex-wrap justify-between gap-x-2"><span className="text-muted-foreground">{t('admin:dashboardCacheReadCoverage')}</span><span className="tabular-nums">{coverage(readCount)}</span></p>
          <p className="flex flex-wrap justify-between gap-x-2"><span className="text-muted-foreground">{t('admin:dashboardCacheWriteCoverage')}</span><span className="tabular-nums">{coverage(writeCount)}</span></p>
          <Tooltip content={t('admin:dashboardMeasuredCacheHint', { read: count(total?.measured_cache_read_tokens), input: count(total?.measured_prompt_tokens) })}>
            <p tabIndex={0} className="flex flex-wrap items-center justify-between gap-x-2 sm:col-span-2"><span className="text-muted-foreground">{t('logs:measuredCacheRate')}</span><span className="tabular-nums">{total?.measured_cache_hit_bp == null ? '—' : formatBp(total.measured_cache_hit_bp, locale)} <span className="text-muted-foreground">{t('admin:dashboardMeasuredSamples', { n: count(total?.measured_cache_hit_requests), total: count(requests) })}</span></span></p>
          </Tooltip>
        </div>
        {partial && requests > 0 && <p className="text-[11px] leading-4 text-muted-foreground">{t('admin:dashboardCacheIncomplete')}</p>}
        {(total?.suspected_test_requests ?? 0) > 0 && <Tooltip content={t('admin:dashboardTestHint')}>
          <p tabIndex={0} className="flex items-start gap-1.5 rounded-md bg-warning/10 px-2 py-1.5 text-[11px] leading-4 text-warning"><FlaskConical aria-hidden size={13} className="mt-0.5 shrink-0" /><span>{t('admin:dashboardTestRecords', { n: count(total?.suspected_test_requests), tokens: count(total?.suspected_test_tokens) })}</span></p>
        </Tooltip>}
        <SourceQuality total={total!} />
      </>}
    </CardContent>
  </Card>
}
