import { useTranslation } from 'react-i18next'
import { Stat } from '@/components/ui/stat'
import { Button } from '@/components/ui/button'
import { formatCount } from '@/lib/money'
import { duration, logMoney } from './types'
import type { LogStats } from './types'

export function LogSummary({ data, loading, error, onRetry }: { data?: LogStats; loading: boolean; error: boolean; onRetry: () => void }) {
  const { t, i18n } = useTranslation(), locale = i18n.language
  const count = (n: number | undefined) => n === undefined ? '—' : formatCount(n, locale)
  const valid = data && typeof data.records === 'number' ? data : undefined
  return <section aria-label={t('logs:summary')} className="shrink-0 space-y-2">
    <div className="flex flex-wrap items-center justify-between gap-1 text-xs text-muted-foreground">
      <p>{t('logs:summaryHint')}</p>
      {error && <span role="status">{t('logs:summaryError')} <Button size="sm" variant="ghost" onClick={onRetry}>{t('common:retry')}</Button></span>}
    </div>
    <div className="grid grid-cols-2 gap-2 md:grid-cols-3 xl:grid-cols-6">
      <Stat compact label={t('logs:netSpend')} value={valid ? logMoney(valid.amount_micro, locale) : '—'} loading={loading} sub={valid?.refunded ? t('logs:refundExcluded', { amount: logMoney(valid.refunded_amount_micro, locale) }) : t('logs:netSpendHint')} />
      <Stat compact label={t('logs:records')} value={count(valid?.records)} loading={loading} sub={valid ? t('logs:statusCounts', { settled: valid.settled, failed: valid.failed, refunded: valid.refunded }) : '—'} />
      <Stat compact label={t('logs:input')} value={count(valid?.prompt_tokens)} loading={loading} sub={t('logs:inputHint')} />
      <Stat compact label={t('logs:output')} value={count(valid?.completion_tokens)} loading={loading} sub={t('logs:outputHint')} />
      <Stat compact label={t('logs:cacheRead')} value={valid && valid.cache_read_samples > 0 ? count(valid.cached_tokens) : '—'} loading={loading} sub={valid ? t('logs:cacheCoverage', { n: valid.cache_read_samples, total: valid.records }) : t('logs:unreported')} />
      <Stat compact label={t('logs:avgTtft')} value={duration(valid?.avg_ttft_ms, locale)} loading={loading} sub={valid && valid.ttft_samples > 0 ? t('logs:timingSamples', { n: valid.ttft_samples, latency: duration(valid.avg_latency_ms, locale) }) : t('logs:noTtftSamples')} />
    </div>
  </section>
}
