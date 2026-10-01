import { useTranslation } from 'react-i18next'
import { useState } from 'react'
import { ArrowUpRight, CircleHelp } from 'lucide-react'
import { Stat } from '@/components/ui/stat'
import { Button } from '@/components/ui/button'
import { Tooltip } from '@/components/ui/tooltip'
import { formatCount } from '@/lib/money'
import { duration, logMoney } from './types'
import type { LogStats } from './types'

const CACHE_SUBSET_FIELDS = new Set([
  'cacheWrite5m', 'cacheWrite1h', 'cacheReadAudio', 'cacheReadImage', 'cacheWriteAudio', 'cacheWriteImage',
])

export function LogSummary({ data, loading, error, onRetry, layout = 'page', onOpenDetails }: {
  data?: LogStats
  loading: boolean
  error: boolean
  onRetry: () => void
  layout?: 'page' | 'panel' | 'strip'
  onOpenDetails?: () => void
}) {
  const { t, i18n } = useTranslation(), locale = i18n.language
  const [expanded, setExpanded] = useState(false)
  const count = (n: number | undefined) => n === undefined ? '—' : formatCount(n, locale)
  const valid = data && typeof data.records === 'number' ? data : undefined
  if (layout === 'strip') {
    const metrics = [
      { label: t('logs:netSpend'), value: valid ? logMoney(valid.amount_micro, locale) : '—', hint: valid?.refunded ? t('logs:refundExcluded', { amount: logMoney(valid.refunded_amount_micro, locale) }) : t('logs:netSpendHint') },
      { label: t('logs:records'), value: count(valid?.records), hint: valid ? t('logs:statusCounts', { settled: valid.settled, failed: valid.failed, refunded: valid.refunded }) : '—' },
      { label: t('logs:input'), value: count(valid?.prompt_tokens), hint: t('logs:inputHint') },
      { label: t('logs:output'), value: count(valid?.completion_tokens), hint: t('logs:outputHint') },
      { label: t('logs:cacheRead'), value: valid && valid.cache_read_samples > 0 ? count(valid.cached_tokens) : '—', hint: valid ? t('logs:cacheCoverage', { n: valid.cache_read_samples, total: valid.records }) : t('logs:unreported') },
      { label: t('logs:avgTtft'), value: duration(valid?.avg_ttft_ms, locale), hint: valid && valid.ttft_samples > 0 ? t('logs:timingSamples', { n: valid.ttft_samples, latency: duration(valid.avg_latency_ms, locale) }) : t('logs:noTtftSamples') },
    ]
    return <section aria-label={t('logs:summary')} aria-busy={loading} className="shrink-0 overflow-hidden rounded-xl border border-border bg-card shadow-card">
      <div className="flex min-h-7 flex-wrap items-center justify-between gap-x-3 border-b border-border/60 px-3 py-1 text-[11px] text-muted-foreground">
        <Tooltip content={t('logs:summaryHint')}><button type="button" className="inline-flex items-center gap-1.5 rounded outline-none focus-visible:ring-2 focus-visible:ring-primary/40">{t('logs:summary')}<CircleHelp aria-hidden className="h-3 w-3" /></button></Tooltip>
        <button type="button" aria-expanded={expanded} onClick={() => setExpanded(!expanded)} className="ml-auto rounded text-primary outline-none focus-visible:ring-2 focus-visible:ring-primary/40">{t('logs:moreMetrics')}</button>
        {error && <span role="status">{t('logs:summaryError')} <Button size="sm" variant="ghost" className="h-6 px-1" onClick={onRetry}>{t('common:retry')}</Button></span>}
      </div>
      <dl className="grid grid-cols-2 sm:grid-cols-3 lg:grid-cols-6">
        {metrics.map((metric) => <div key={metric.label} className="min-w-0 border-border/60 px-3 py-2 even:border-l sm:[&:nth-child(3n+2)]:border-l sm:[&:nth-child(3n)]:border-l lg:[&:not(:first-child)]:border-l">
          <dt className="text-xs text-muted-foreground"><Tooltip content={metric.hint}>
            <button type="button" className="max-w-full truncate rounded text-left outline-none focus-visible:ring-2 focus-visible:ring-primary/40">{metric.label}</button>
          </Tooltip></dt>
          <dd className="truncate text-lg leading-6 font-semibold tabular-nums" title={metric.value}>{loading ? <span className="my-1 block h-4 w-16 animate-pulse rounded bg-muted" /> : metric.value}</dd>
        </div>)}
      </dl>
      <ExtraMetrics data={valid} expanded={expanded} />
    </section>
  }
  const interaction = onOpenDetails ? {
    onClick: onOpenDetails,
    layout: 'stacked' as const,
    className: 'min-w-0 outline-none focus-visible:ring-2 focus-visible:ring-primary/40',
    aside: <ArrowUpRight aria-hidden className="absolute top-3 right-3 h-3.5 w-3.5 text-muted-foreground" />,
  } : {}
  return <section aria-label={t('logs:summary')} aria-busy={loading} className="shrink-0 space-y-2">
    <div className="flex flex-wrap items-center justify-between gap-1 text-xs text-muted-foreground">
      <p>{t('logs:summaryHint')}</p>
      {error && <span role="status">{t('logs:summaryError')} <Button size="sm" variant="ghost" onClick={onRetry}>{t('common:retry')}</Button></span>}
    </div>
    <div className={layout === 'panel' ? 'grid grid-cols-1 gap-2 min-[360px]:grid-cols-2' : 'grid grid-cols-2 gap-2 md:grid-cols-3 xl:grid-cols-6'}>
      <Stat {...interaction} compact label={t('logs:netSpend')} value={valid ? logMoney(valid.amount_micro, locale) : '—'} loading={loading} sub={valid?.refunded ? t('logs:refundExcluded', { amount: logMoney(valid.refunded_amount_micro, locale) }) : t('logs:netSpendHint')} />
      <Stat {...interaction} compact label={t('logs:records')} value={count(valid?.records)} loading={loading} sub={valid ? t('logs:statusCounts', { settled: valid.settled, failed: valid.failed, refunded: valid.refunded }) : '—'} />
      <Stat {...interaction} compact label={t('logs:input')} value={count(valid?.prompt_tokens)} loading={loading} sub={t('logs:inputHint')} />
      <Stat {...interaction} compact label={t('logs:output')} value={count(valid?.completion_tokens)} loading={loading} sub={t('logs:outputHint')} />
      <Stat {...interaction} compact label={t('logs:cacheRead')} value={valid && valid.cache_read_samples > 0 ? count(valid.cached_tokens) : '—'} loading={loading} sub={valid ? t('logs:cacheCoverage', { n: valid.cache_read_samples, total: valid.records }) : t('logs:unreported')} />
      <Stat {...interaction} compact label={t('logs:avgTtft')} value={duration(valid?.avg_ttft_ms, locale)} loading={loading} sub={valid && valid.ttft_samples > 0 ? t('logs:timingSamples', { n: valid.ttft_samples, latency: duration(valid.avg_latency_ms, locale) }) : t('logs:noTtftSamples')} />
    </div>
    <ExtraMetrics data={valid} />
  </section>
}

export function ExtraMetrics({ data, expanded }: { data?: Partial<LogStats>; expanded?: boolean }) {
  const { t, i18n } = useTranslation(), locale = i18n.language
  if (expanded === false) return null
  const value = (n: number | null | undefined) => n == null ? '—' : n.toLocaleString(locale)
  const metrics: [string, number | null | undefined, number | undefined][] = [
    ['cacheWrite', data?.cache_write_tokens, data?.cache_write_samples],
    ['cacheWrite5m', data?.cache_write_5m_tokens, data?.cache_write_ttl_samples],
    ['cacheWrite1h', data?.cache_write_1h_tokens, data?.cache_write_ttl_samples],
    ['reasoning', data?.reasoning_tokens, undefined],
    ['audioInput', data?.audio_prompt_tokens, data?.audio_prompt_samples],
    ['imageInput', data?.image_prompt_tokens, data?.image_prompt_samples],
    ['audioOutput', data?.audio_completion_tokens, data?.audio_completion_samples],
    ['imageOutput', data?.image_completion_tokens, data?.image_completion_samples],
    ['cacheReadAudio', data?.cache_read_audio_tokens, data?.cache_read_modal_samples],
    ['cacheReadImage', data?.cache_read_image_tokens, data?.cache_read_modal_samples],
    ['cacheWriteAudio', data?.cache_write_audio_tokens, data?.cache_write_modal_samples],
    ['cacheWriteImage', data?.cache_write_image_tokens, data?.cache_write_modal_samples],
  ]
  const visibleMetrics = metrics.filter(([key, n, samples]) => !CACHE_SUBSET_FIELDS.has(key)
    || (n != null && (samples === undefined || samples > 0)))
  const content = <div className="space-y-3 py-3">
      <dl className="grid grid-cols-2 gap-3 sm:grid-cols-4">
        {visibleMetrics.map(([key, n, samples]) => <div key={key} className="min-w-0 space-y-1">
          <dt className="text-muted-foreground">{t(`logs:${key}`)}</dt><dd className="font-medium tabular-nums">{value(n)}</dd>
          {samples !== undefined && <dd className="text-[11px] text-muted-foreground">{t('logs:cacheCoverage', { n: samples, total: data?.records })}</dd>}
        </div>)}
      </dl>
      <p className="text-[11px] leading-5 text-muted-foreground">{t('logs:tokenHint')} {visibleMetrics.some(([key]) => CACHE_SUBSET_FIELDS.has(key)) && t('logs:cacheSubsetsHint')}</p>
      <div className="flex flex-wrap items-center gap-2 rounded-md bg-muted/30 px-2 py-1.5">
        <span className="text-muted-foreground">{t('logs:measuredCacheRate')}</span>
        <span className="font-medium tabular-nums">{data?.measured_cache_hit_bp == null ? '—' : `${(data.measured_cache_hit_bp / 100).toLocaleString(locale, { maximumFractionDigits: 2 })}%`}</span>
        <span className="text-muted-foreground">{t('logs:cacheCoverage', { n: data?.measured_cache_hit_requests ?? 0, total: data?.records ?? 0 })}</span>
      </div>
      {data?.token_provenance && <div className="grid gap-3 sm:grid-cols-2">
        {(['prompt', 'completion'] as const).map((axis) => <div key={axis} className="space-y-2 rounded-md border border-border p-2">
          <p className="font-medium">{t('logs:usageSource')} · {t(axis === 'prompt' ? 'logs:input' : 'logs:output')}</p>
          <dl className="space-y-1">{(['upstream', 'estimated', 'local_override', 'unknown'] as const).map((source) => {
            const counts = data.token_provenance?.[axis]?.[source]
            return <div key={source} className="flex flex-wrap justify-between gap-2"><dt className="text-muted-foreground">{t(`logs:source_${source}`)}</dt><dd className="tabular-nums">{t('logs:sourceCounts', { n: counts?.requests ?? '—', tokens: value(counts?.tokens) })}</dd></div>
          })}</dl>
        </div>)}
      </div>}
    </div>
  return expanded === undefined ? <details className="border-t border-border/60 px-3 text-xs">
    <summary className="cursor-pointer py-1.5 text-muted-foreground outline-none focus-visible:ring-2 focus-visible:ring-primary/40">{t('logs:moreMetrics')}</summary>
    {content}
  </details> : <div className="border-t border-border/60 px-3 text-xs">{content}</div>
}
