import { useTranslation } from 'react-i18next'
import { FieldGroup } from '@/components/ui/drawer'
import { duration } from './types'
import type { LogRow } from './types'

export function LogPerformance({ row }: { row: Pick<LogRow, 'is_stream' | 'ttft_ms' | 'latency_ms'> }) {
  const { t, i18n } = useTranslation()
  return <div className="text-xs leading-4 whitespace-nowrap">
    <div><span className="mr-2 text-muted-foreground">{t('logs:ttft')}</span>{row.is_stream ? duration(row.ttft_ms, i18n.language) : t('logs:nonStreaming')}</div>
    <div><span className="mr-2 text-muted-foreground">{t('logs:totalShort')}</span>{duration(row.latency_ms, i18n.language)}</div>
  </div>
}

/** Average throughput includes waiting; generation speed requires measured streaming timings. */
export function LogPerformanceDetails({ row }: { row: Pick<LogRow, 'is_stream' | 'ttft_ms' | 'latency_ms' | 'usage'> }) {
  const { t, i18n } = useTranslation(), locale = i18n.language
  const tokens = row.usage.completion_tokens
  const total = row.latency_ms
  const valid = Number.isFinite(tokens) && tokens >= 0 && total != null && Number.isFinite(total) && total > 0
  const average = valid ? tokens * 1000 / total : null
  const ttft = row.ttft_ms
  const generation = valid && row.is_stream && ttft != null && Number.isFinite(ttft) && ttft >= 0 && total > ttft && tokens > 0
    ? tokens * 1000 / (total - ttft) : null
  const speed = (value: number | null) => value == null || !Number.isFinite(value) ? '—' : `${value.toLocaleString(locale, { maximumFractionDigits: 1 })} tok/s`
  const field = (label: string, value: string) => <div className="min-w-0 space-y-1"><dt className="text-xs text-muted-foreground">{label}</dt><dd className="break-words text-sm tabular-nums">{value}</dd></div>
  return <FieldGroup title={t('logs:performance')} hint={t('logs:speedHint')}>
    <dl className="grid grid-cols-2 gap-3 sm:grid-cols-4">
      {field(t('logs:ttft'), row.is_stream ? duration(ttft, locale) : t('logs:notApplicable'))}
      {field(t('logs:totalLatency'), duration(total, locale))}
      {field(t('logs:averageSpeed'), speed(average))}
      {field(t('logs:speed'), speed(generation))}
    </dl>
  </FieldGroup>
}
