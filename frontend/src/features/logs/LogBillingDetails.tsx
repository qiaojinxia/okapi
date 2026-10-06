import { Receipt } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { DetailSection } from './detail-ui'
import { formatRatio, formatUnitPrice } from '@/lib/money'
import { billingLines } from './types'
import type { LogRow } from './types'

export function LogBillingDetails({ row, status }: {
  row: Pick<LogRow, 'usage' | 'usage_details_recorded' | 'pricing_snapshot' | 'endpoint'>; status?: number | null
}) {
  const { t, i18n } = useTranslation(), locale = i18n.language
  const s = row.pricing_snapshot, lines = billingLines(row)
  const characters = (row.usage.input_unit ?? s?.input_unit) === 'characters'
  const audioDuration = row.endpoint?.startsWith('/v1/audio/') === true
  const value = (n: number | null | undefined) => n == null ? '—' : n.toLocaleString(locale)
  return (
      <DetailSection icon={Receipt} title={t('logs:billingDetails')}>
        {status === 30 && <p className="text-xs text-muted-foreground">{t('logs:refundedHint')}</p>}
        {lines.length > 0 && <>
          <div className="overflow-x-auto rounded-lg border border-border/70">
            <table className="w-full text-[13px] leading-5 tabular-nums" aria-label={t('logs:billingLines')}>
              <thead className="bg-muted/60 text-xs text-muted-foreground"><tr>{['segment', 'quantity', characters ? 'characterPriceUnit' : 'snapshotUnit', 'referenceAmount'].map((key, i) => <th key={key} className={`px-3 py-2 font-medium ${i ? 'text-right' : 'text-left'}`}>{t(`logs:${key}`)}</th>)}</tr></thead>
              <tbody className="divide-y divide-border/60">{lines.map((line) => <tr key={line.name}>
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
          {s.mode === 'per_call' && <p>{t('logs:perCallHint', { price: s.per_call_price_usd ?? '—', n: audioDuration ? 1 : s.media_units ?? 1 })}</p>}
          {s.media_units != null && <p>{t(audioDuration ? 'logs:audioDuration' : row.endpoint === '/v1/videos' ? 'logs:billedVideoSeconds' : 'logs:mediaQuantity')} · {s.media_units.toLocaleString(locale)}{audioDuration || row.endpoint === '/v1/videos' ? ' s' : ''}</p>}
          <div className="flex flex-wrap gap-1.5 rounded-lg border border-border/70 bg-muted/25 p-3">
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
      </DetailSection>
  )
}
