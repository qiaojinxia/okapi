import { Gauge } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { Tooltip } from '@/components/ui/tooltip'
import { cn } from '@/lib/utils'
import { DetailSection, InfoGrid, StatTile } from './detail-ui'
import { formatOutputRate, outputRates } from './performance'
import { duration } from './types'
import type { LogRow } from './types'

export function LogPerformance({ row }: { row: Pick<LogRow, 'is_stream' | 'ttft_ms' | 'latency_ms' | 'usage'> }) {
  const { t, i18n } = useTranslation()
  const { average } = outputRates(row), rate = formatOutputRate(average, i18n.language, true)
  return <div data-slot="log-performance" className="ml-auto grid w-58 grid-cols-[6rem_minmax(0,1fr)] items-center gap-x-2 text-xs leading-4 tabular-nums whitespace-nowrap">
    <Tooltip className="w-full" content={`${average == null ? t('logs:outputThroughputMissing') : `${t('charts:throughput')} ${formatOutputRate(average, i18n.language)} ·`} ${t('logs:outputThroughputHint')}`}>
      <span data-slot="output-throughput" data-state={average == null ? 'missing' : 'measured'} tabIndex={0}
        aria-label={`${t('charts:throughput')} ${rate}`}
        className={cn('inline-flex h-6 w-full items-center justify-center gap-1 rounded px-1.5 text-[11px] font-medium outline-none focus-visible:ring-2 focus-visible:ring-primary/40', average == null ? 'border border-dashed border-border text-muted-foreground' : 'bg-primary/8 text-foreground')}>
        <Gauge aria-hidden className={cn('h-3 w-3 shrink-0', average != null && 'text-primary')} />{rate}
      </span>
    </Tooltip>
    <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-2 text-right">
      <dt className="text-left text-muted-foreground">{t('logs:ttft')}</dt>
      <dd>{row.is_stream ? duration(row.ttft_ms, i18n.language) : t('logs:nonStreaming')}</dd>
      <dt className="text-left text-muted-foreground">{t('logs:totalShort')}</dt>
      <dd>{duration(row.latency_ms, i18n.language)}</dd>
    </dl>
  </div>
}

/** Average throughput includes waiting; generation speed requires measured streaming timings. */
export function LogPerformanceDetails({ row }: { row: Pick<LogRow, 'is_stream' | 'ttft_ms' | 'latency_ms' | 'usage'> }) {
  const { t, i18n } = useTranslation(), locale = i18n.language
  const total = row.latency_ms
  const { average, generation } = outputRates(row)
  const ttft = row.ttft_ms
  const field = (label: string, value: string) => <StatTile label={label}>{value}</StatTile>
  return <DetailSection icon={Gauge} title={t('logs:performance')} hint={t('logs:speedHint')}>
    <InfoGrid cols={4} className="gap-2.5">
      {field(t('logs:ttft'), row.is_stream ? duration(ttft, locale) : t('logs:notApplicable'))}
      {field(t('logs:totalLatency'), duration(total, locale))}
      {field(t('logs:averageSpeed'), formatOutputRate(average, locale))}
      {field(t('logs:speed'), formatOutputRate(generation, locale))}
    </InfoGrid>
  </DetailSection>
}
