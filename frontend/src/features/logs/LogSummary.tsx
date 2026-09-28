import { useTranslation } from 'react-i18next'
import { ArrowUpRight } from 'lucide-react'
import { Stat } from '@/components/ui/stat'
import { Button } from '@/components/ui/button'
import { formatCount } from '@/lib/money'
import { duration, logMoney } from './types'
import type { LogStats } from './types'

export function LogSummary({ data, loading, error, onRetry, layout = 'page', onOpenDetails }: {
  data?: LogStats
  loading: boolean
  error: boolean
  onRetry: () => void
  layout?: 'page' | 'panel'
  onOpenDetails?: () => void
}) {
  const { t, i18n } = useTranslation(), locale = i18n.language
  const count = (n: number | undefined) => n === undefined ? '—' : formatCount(n, locale)
  const valid = data && typeof data.records === 'number' ? data : undefined
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
  </section>
}
