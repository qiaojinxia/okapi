import { Gauge } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { DetailSection, InfoGrid, StatTile } from './detail-ui'
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
  const valid = row.usage.input_unit !== 'characters' && Number.isFinite(tokens) && tokens >= 0 && total != null && Number.isFinite(total) && total > 0
  const average = valid ? tokens * 1000 / total : null
  const ttft = row.ttft_ms
  const generation = valid && row.is_stream && ttft != null && Number.isFinite(ttft) && ttft >= 0 && total > ttft && tokens > 0
    ? tokens * 1000 / (total - ttft) : null
  const speed = (value: number | null) => value == null || !Number.isFinite(value) ? '—' : `${value.toLocaleString(locale, { maximumFractionDigits: 1 })} tok/s`
  const field = (label: string, value: string) => <StatTile label={label}>{value}</StatTile>
  return <DetailSection icon={Gauge} title={t('logs:performance')} hint={t('logs:speedHint')}>
    <InfoGrid cols={4} className="gap-2.5">
      {field(t('logs:ttft'), row.is_stream ? duration(ttft, locale) : t('logs:notApplicable'))}
      {field(t('logs:totalLatency'), duration(total, locale))}
      {field(t('logs:averageSpeed'), speed(average))}
      {field(t('logs:speed'), speed(generation))}
    </InfoGrid>
  </DetailSection>
}
