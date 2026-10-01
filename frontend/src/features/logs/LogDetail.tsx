import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { CopyText } from '@/components/ui/copy-button'
import { Drawer, FieldGroup } from '@/components/ui/drawer'
import { formatRatio, formatUnitPrice } from '@/lib/money'
import { billingLines, billingStatus, duration, logMoney, netAmount } from './types'
import { TokenBreakdown } from './TokenBreakdown'
import type { LogRow } from './types'

export function LogStatus({ row }: { row: LogRow }) {
  const { t } = useTranslation()
  return <Badge dot variant={row.status === 20 ? 'success' : row.status === 40 ? 'destructive' : row.status === 30 ? 'info' : 'muted'}>{t(`logs:${billingStatus(row.status)}`)}</Badge>
}

export function LogDetail({ row, onClose, id, timezone }: { row: LogRow | null; onClose: () => void; id: string; timezone: string }) {
  const { t, i18n } = useTranslation(), locale = i18n.language
  if (!row) return null
  const s = row.pricing_snapshot, lines = billingLines(row)
  const value = (n: number | null | undefined) => n == null ? t(row.usage_details_recorded ? 'logs:unreported' : 'logs:notRecorded') : n.toLocaleString(locale)
  const field = (label: string, content: React.ReactNode) => <div className="min-w-0 space-y-1"><dt className="text-xs text-muted-foreground">{label}</dt><dd className="break-words text-sm tabular-nums">{content}</dd></div>
  const measuredTtft = row.is_stream ? row.ttft_ms : null
  const speed = row.is_stream && row.ttft_ms != null && row.ttft_ms >= 0 && row.latency_ms != null && row.latency_ms > row.ttft_ms && row.usage.completion_tokens > 0
    ? row.usage.completion_tokens * 1000 / (row.latency_ms - row.ttft_ms) : null
  const errorKey = `errors:${row.error_code}`
  const errorHints: Record<string, string> = { no_available_channel: 'logs:errorNoChannel', unsupported_endpoint: 'logs:errorEndpoint', insufficient_quota: 'logs:errorQuota', rate_limited: 'logs:errorRateLimit' }
  return <Drawer open onClose={onClose} title={t('logs:detailTitle')} description={t('logs:detailHint')} size="lg">
    <div id={id}>
      <div className="mb-5 grid grid-cols-2 gap-4 rounded-lg border border-border bg-muted/30 p-4">
        <div><p className="text-xs text-muted-foreground">{t('logs:netSpend')}</p><p className="mt-1 text-2xl font-semibold tabular-nums">{logMoney(netAmount(row), locale)}</p></div>
        <dl className="space-y-2 text-xs">
          <div className="flex justify-between gap-2"><dt>{t('logs:beforeDiscount')}</dt><dd>{logMoney(row.original_amount_micro, locale)}</dd></div>
          <div className="flex justify-between gap-2"><dt>{t('logs:pricingDiscount')}</dt><dd>{logMoney(row.discount_micro, locale)}</dd></div>
          {row.status === 30 && <div className="flex justify-between gap-2 text-info"><dt>{t('logs:refundAmount')}</dt><dd>{logMoney(row.amount_micro, locale)}</dd></div>}
        </dl>
      </div>
      <FieldGroup title={t('logs:requestInfo')}>
        <dl className="grid grid-cols-2 gap-x-5 gap-y-4">
          {field(t('logs:billingState'), <LogStatus row={row} />)}
          {field(`${t('logs:time')} · ${timezone}`, new Date(row.created_at).toLocaleString(locale, { timeZone: timezone }))}
          {field(t('logs:billedModel'), row.model)}
          {field(t('logs:requestedModel'), row.requested_model || t('logs:notRecorded'))}
          {field(t('portal:keys'), row.key_name || (row.api_key_id ? `#${row.api_key_id}` : '—'))}
          {field(t('logs:endpoint'), row.endpoint || t('logs:notRecorded'))}
          {field(t('logs:responseMode'), t(row.is_stream ? 'logs:streaming' : 'logs:nonStreaming'))}
          {field(t('logs:funding'), row.pool === 0 ? t('logs:wallet') : row.pool === 1 ? t('logs:subscription') : t('logs:notRecorded'))}
        </dl>
        <div className="rounded-lg bg-muted/50 p-3 text-xs"><p className="mb-2 text-muted-foreground">{t('admin:logsRequestId')}</p><CopyText value={row.request_id} className="break-all" /></div>
        {row.error_code && <div className="space-y-2 rounded-lg border border-destructive/20 bg-destructive/5 p-3 text-sm" role="status">
          <p className="font-medium">{errorHints[row.error_code] ? t(errorHints[row.error_code]) : i18n.exists(errorKey) ? t(errorKey, { param: '' }) : t('logs:failureHint')}</p>
          <p className="font-mono text-xs">{row.error_code}</p>
          <p className="text-xs text-muted-foreground">{t('logs:supportHint')}</p>
        </div>}
      </FieldGroup>
      <FieldGroup title={t('logs:performance')} hint={t('logs:speedHint')}>
        <dl className="grid grid-cols-3 gap-3">
          {field(t('logs:ttft'), row.is_stream ? duration(measuredTtft, locale) : t('logs:notApplicable'))}
          {field(t('logs:totalLatency'), duration(row.latency_ms, locale))}
          {field(t('logs:speed'), speed === null ? '—' : `${speed.toLocaleString(locale, { maximumFractionDigits: 1 })} tok/s`)}
        </dl>
      </FieldGroup>
      <TokenBreakdown usage={row.usage} recorded={row.usage_details_recorded} />
      <FieldGroup title={t('logs:billingDetails')}>
        {row.status === 30 && <p className="text-xs text-muted-foreground">{t('logs:refundedHint')}</p>}
        {lines.length > 0 && <>
          <div className="overflow-x-auto rounded-lg border border-border">
            <table className="w-full text-xs tabular-nums" aria-label={t('logs:billingLines')}>
              <thead className="bg-muted text-muted-foreground"><tr>{['segment', 'quantity', 'snapshotUnit', 'referenceAmount'].map((key, i) => <th key={key} className={`px-3 py-2 font-medium ${i ? 'text-right' : 'text-left'}`}>{t(`logs:${key}`)}</th>)}</tr></thead>
              <tbody className="divide-y divide-border">{lines.map((line) => <tr key={line.name}>
                <td className="whitespace-nowrap px-3 py-2.5">{t(`logs:${line.name}`)}</td>
                <td className="px-3 py-2.5 text-right">{value(line.quantity)}</td>
                <td className="px-3 py-2.5 text-right">{formatUnitPrice(line.unitMicro, locale)}</td>
                <td className="px-3 py-2.5 text-right">{formatUnitPrice(line.amountMicro, locale)}</td>
              </tr>)}</tbody>
            </table>
          </div>
          <p className="text-xs leading-5 text-muted-foreground">{t('logs:referenceHint')}</p>
        </>}
        {s ? <>
          {s.mode === 'per_call' && <p className="text-sm">{t('logs:perCallHint', { price: s.per_call_price_usd ?? '—', n: s.media_units ?? 1 })}</p>}
          <div className="flex flex-wrap gap-1.5">
            <Badge variant="muted">{t('logs:mode')} {t(`logs:mode_${s.mode}`, { defaultValue: s.mode })}</Badge>
            {s.epoch != null && <Badge variant="muted">{t('logs:pricingVersion')} {s.epoch}</Badge>}
            {s.service_tier && <Badge variant="info">{t('logs:serviceTier')} {s.service_tier}{s.tier_ratio != null ? ` ×${formatRatio(s.tier_ratio)}` : ''}</Badge>}
            {s.mode !== 'per_call' && <Badge variant="muted">{t('admin:pricingBasePreview', { price: s.base_price_per_1m_usd ?? 2 })}</Badge>}
            <Badge variant="muted">{t('logs:group')} {s.group} ×{formatRatio(s.group_ratio)}</Badge>
            <Badge variant="muted">{t('logs:userMultiplier')} ×{formatRatio(s.user_multiplier)}</Badge>
            {s.model_ratio != null && <Badge variant="muted">{t('admin:modelRatio')} ×{formatRatio(s.model_ratio)}</Badge>}
            {s.completion_ratio != null && <Badge variant="muted">{t('admin:completionRatio')} ×{formatRatio(s.completion_ratio)}</Badge>}
            {s.cache_ratio != null && <Badge variant="muted">{t('logs:cacheRead')} ×{formatRatio(s.cache_ratio)}</Badge>}
            {s.cache_write_ratio != null && <Badge variant="muted">{t('logs:cacheWrite')} ×{formatRatio(s.cache_write_ratio)}</Badge>}
            {(s.rules ?? []).map((rule, i) => <Badge key={`${rule.code}-${i}`}>{rule.code} ×{formatRatio(rule.multiplier)}</Badge>)}
          </div>
          {!lines.length && s.mode !== 'per_call' && <p className="text-xs text-muted-foreground">{t('logs:breakdownUnavailable')}</p>}
        </> : <p className="text-xs text-muted-foreground">{t('logs:noSnapshot')}</p>}
      </FieldGroup>
    </div>
  </Drawer>
}
