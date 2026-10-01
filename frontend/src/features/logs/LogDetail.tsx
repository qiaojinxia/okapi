import { FileText } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Drawer } from '@/components/ui/drawer'
import { DetailSection, IdRow, InfoGrid, InfoItem } from './detail-ui'
import { billingStatus, logMoney, netAmount } from './types'
import { LogPerformanceDetails } from './LogPerformance'
import { LogErrorDetails } from './LogErrorDetails'
import { TokenBreakdown } from './TokenBreakdown'
import { LogBillingDetails } from './LogBillingDetails'
import { LogRequestDetails } from './LogRequestDetails'
import type { LogRow } from './types'

export function LogStatus({ row }: { row: Pick<LogRow, 'status' | 'is_error'> }) {
  const { t } = useTranslation()
  return <span className="inline-flex flex-wrap gap-1">{row.is_error && row.status !== 40 && <Badge dot variant="destructive">{t('logs:failed')}</Badge>}<Badge dot variant={row.status === 20 ? 'success' : row.status === 40 ? 'destructive' : row.status === 30 ? 'info' : 'muted'}>{t(`logs:${billingStatus(row.status)}`)}</Badge></span>
}

export function LogDetail({ row, onClose, id, timezone }: { row: LogRow | null; onClose: () => void; id: string; timezone: string }) {
  const { t, i18n } = useTranslation(), locale = i18n.language
  if (!row) return null
  const field = (label: string, content: React.ReactNode) => <InfoItem label={label}>{content}</InfoItem>
  return <Drawer open onClose={onClose} title={t('logs:detailTitle')} description={t('logs:detailHint')} size="lg">
    <div id={id} className="flex flex-col gap-4">
      <div className="empty:hidden"><LogErrorDetails failed={row.is_error || row.status === 40} code={row.error_code} diagnostics={row.diagnostics} /></div>
      <div className="grid gap-4 overflow-hidden rounded-xl border border-border bg-gradient-to-br from-primary/10 via-card to-card p-5 shadow-xs sm:grid-cols-[1fr_auto] sm:items-end">
        <div className="min-w-0"><p className="text-xs font-medium text-muted-foreground">{t('logs:netSpend')}</p><p className="mt-1.5 text-3xl leading-9 font-semibold tracking-tight tabular-nums">{logMoney(netAmount(row), locale)}</p></div>
        <dl className="grid min-w-48 gap-1.5 rounded-lg bg-card/75 p-3 text-xs ring-1 ring-border/60">
          <div className="flex justify-between gap-6"><dt className="text-muted-foreground">{t('logs:beforeDiscount')}</dt><dd className="font-medium tabular-nums">{logMoney(row.original_amount_micro, locale)}</dd></div>
          <div className="flex justify-between gap-6"><dt className="text-muted-foreground">{t('logs:pricingDiscount')}</dt><dd className="font-medium tabular-nums">{logMoney(row.discount_micro, locale)}</dd></div>
          {row.status === 30 && <div className="flex justify-between gap-6 text-info"><dt>{t('logs:refundAmount')}</dt><dd className="font-medium tabular-nums">{logMoney(row.amount_micro, locale)}</dd></div>}
        </dl>
      </div>
      <DetailSection icon={FileText} title={t('logs:requestInfo')}>
        <InfoGrid cols={2}>
          {field(t('logs:billingState'), <LogStatus row={row} />)}
          {field(`${t('logs:time')} · ${timezone}`, new Date(row.created_at).toLocaleString(locale, { timeZone: timezone }))}
          {field(t('logs:billedModel'), row.model)}
          {field(t('logs:requestedModel'), row.requested_model || t('logs:notRecorded'))}
          {field(t('portal:keys'), row.key_name || (row.api_key_id ? `#${row.api_key_id}` : '—'))}
          {field(t('logs:endpoint'), row.endpoint || t('logs:notRecorded'))}
          {field(t('logs:responseMode'), t(row.is_stream ? 'logs:streaming' : 'logs:nonStreaming'))}
          {field(t('logs:funding'), row.pool === 0 ? t('logs:wallet') : row.pool === 1 ? t('logs:subscription') : t('logs:notRecorded'))}
        </InfoGrid>
        <dl className="grid gap-2">
          <IdRow label={t('admin:logsRequestId')} value={row.request_id} />
          {row.upstream_request_id && <IdRow label={t('admin:logsUpstreamId')} value={row.upstream_request_id} />}
        </dl>
      </DetailSection>
      <LogRequestDetails row={row} />
      <LogPerformanceDetails row={row} />
      <TokenBreakdown usage={row.usage} recorded={row.usage_details_recorded} />
      <LogBillingDetails row={row} status={row.status} />
    </div>
  </Drawer>
}
