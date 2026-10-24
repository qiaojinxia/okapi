import { Link } from '@tanstack/react-router'
import { CircleHelp, FlaskConical } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { ErrorState, LoadingState } from '@/components/ui/state'
import { Tooltip } from '@/components/ui/tooltip'
import type { CubeMetrics, TokenSource } from '@/features/analytics/types'
import { segments } from '@/features/portal-overview/token-segments'
import { describeError } from '@/lib/i18n'
import { formatBp, formatCount } from '@/lib/money'
import { useDashboardUsage } from './data'
import type { DateRange } from '@/components/ui/date-range'
import { dashboardPeriodLabel, dashboardSearch } from './period'
import { HEAD_LINK } from './distribution-layout'
import { ShareBar } from './ShareBar'

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
      <div role="group" className="mt-2 grid grid-cols-2 gap-2" aria-label={t('logs:usageSource')}>
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

/// Token 构成：把已记录的用量拆成互斥的几段（其他输入、缓存读取、缓存写入、输出、推理），
/// 版式和模型 / 渠道排行一致——名称、数量、占比，下面一条占比条。缓存上报覆盖率和来源详情放在底部。
export function TokenSummary({ days, range }: { days: number; range?: DateRange | null }) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language, query = useDashboardUsage(days, range)
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
  return <div role="region" aria-label={t('portal:tokenSnapshot')} className="flex min-w-0 flex-col gap-3">
    <div className="flex flex-wrap items-baseline justify-between gap-x-2 gap-y-1">
      <div className="flex min-w-0 flex-col gap-0.5">
        <Tooltip content={t('admin:dashboardTokenHint')}>
          <span tabIndex={0} className="rounded text-xs text-muted-foreground outline-none focus-visible:ring-2 focus-visible:ring-primary/40">{dashboardPeriodLabel(days, range, t)} · {t('portal:tokenSnapshot')}</span>
        </Tooltip>
        <span className="text-lg leading-7 font-bold tabular-nums" title={collected ? sum.toLocaleString(locale) : undefined}>
          {query.isSuccess && collected ? <>{count(sum)} <span className="text-xs font-normal text-muted-foreground">{t('common:tokens')}</span></> : '—'}
        </span>
      </div>
      <Link to="/admin/stats" search={{ ...dashboardSearch(days, range), measure: 'tokens' }} className={HEAD_LINK}>{t('admin:dashboardViewAll')}<span aria-hidden>&nbsp;→</span></Link>
    </div>
    {query.isError ? <ErrorState message={describeError(query.error)} onRetry={() => void query.refetch()} /> : query.isPending ? <LoadingState /> : !collected ? <p className="py-6 text-xs text-muted-foreground">{t('analysis:notCollected')}</p> : <>
      <div className="flex flex-wrap items-center gap-x-2 gap-y-1 text-[11px]">
        <Tooltip content={t('admin:dashboardRecordedTokenHint')}><Badge variant="muted" tabIndex={0} className="min-h-5 text-[11px]">{t('admin:dashboardRecordedTokens')}</Badge></Tooltip>
        <span className="text-muted-foreground">{t('admin:dashboardTokenTotal', { input: count(total?.prompt_tokens), output: count(total?.completion_tokens) })}</span>
      </div>
      <dl data-slot="token-segments" className="flex flex-col gap-3">
        {parts.map((part) => {
          const known = part.key === 'cached' ? cached > 0 || (readCount ?? 0) > 0 : part.key === 'write' ? writes != null : part.key === 'reasoning' ? reasoning > 0 || (total?.token_detail_observations?.reasoning_tokens?.observed_records ?? 0) > 0 : true
          const incomplete = part.key === 'cached' ? !readComplete : part.key === 'write' ? !writeComplete : false
          const share = known && sum > 0 ? Math.round(part.value / sum * 10000) : null
          return <div key={part.key} className="flex min-w-0 flex-col gap-1.5">
            <div className="flex items-baseline gap-2 text-[13px]">
              <dt className="flex min-w-0 flex-1 items-center gap-2 font-medium"><span aria-hidden className={`size-2 shrink-0 rounded-[2px] ${part.className}`} />{labels[part.key]}</dt>
              <dd className="shrink-0 text-xs text-muted-foreground tabular-nums" title={known ? part.value.toLocaleString(locale) : undefined}>{known ? count(part.value) : '—'}{known && incomplete && <span className="ml-1 text-[10px] text-warning">{t('admin:dashboardPartialTokens')}</span>}</dd>
              <dd className="w-[52px] shrink-0 text-right font-semibold tabular-nums">{share == null ? '—' : formatBp(share, locale)}</dd>
            </div>
            <ShareBar bp={share} className={part.className} />
          </div>
        })}
      </dl>
      <div className="flex flex-col gap-2">
        <div role="group" className="grid gap-x-3 gap-y-1 rounded-md bg-muted/40 px-2 py-1.5 text-[11px] sm:grid-cols-2" aria-label={t('admin:dashboardCacheCoverage')}>
          <p className="flex flex-wrap justify-between gap-x-2"><span className="text-muted-foreground">{t('admin:dashboardCacheReadCoverage')}</span><span className="tabular-nums">{coverage(readCount)}</span></p>
          <p className="flex flex-wrap justify-between gap-x-2"><span className="text-muted-foreground">{t('admin:dashboardCacheWriteCoverage')}</span><span className="tabular-nums">{coverage(writeCount)}</span></p>
          <Tooltip className="sm:col-span-2" content={t('admin:dashboardMeasuredCacheHint', { read: count(total?.measured_cache_read_tokens), input: count(total?.measured_prompt_tokens) })}>
            <p tabIndex={0} className="flex w-full flex-wrap items-center justify-between gap-x-2"><span className="text-muted-foreground">{t('logs:measuredCacheRate')}</span><span className="tabular-nums">{total?.measured_cache_hit_bp == null ? '—' : formatBp(total.measured_cache_hit_bp, locale)} <span className="text-muted-foreground">{t('admin:dashboardMeasuredSamples', { n: count(total?.measured_cache_hit_requests), total: count(requests) })}</span></span></p>
          </Tooltip>
        </div>
        {partial && requests > 0 && <p className="text-[11px] leading-4 text-muted-foreground">{t('admin:dashboardCacheIncomplete')}</p>}
        {(total?.suspected_test_requests ?? 0) > 0 && <Tooltip content={t('admin:dashboardTestHint')}>
          <p tabIndex={0} className="flex items-start gap-1.5 rounded-md bg-warning/10 px-2 py-1.5 text-[11px] leading-4 text-warning"><FlaskConical aria-hidden size={13} className="mt-0.5 shrink-0" /><span>{t('admin:dashboardTestRecords', { n: count(total?.suspected_test_requests), tokens: count(total?.suspected_test_tokens) })}</span></p>
        </Tooltip>}
        <SourceQuality total={total!} />
      </div>
    </>}
  </div>
}
