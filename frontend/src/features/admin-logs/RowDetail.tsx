import { useTranslation } from 'react-i18next'
import { Fingerprint, KeyRound, Route, Server, User, Wallet } from 'lucide-react'
import type { LucideIcon } from 'lucide-react'
import { LogPerformanceDetails } from '@/features/logs/LogPerformance'
import { LogErrorDetails } from '@/features/logs/LogErrorDetails'
import { DetailAmount, DetailBody, DetailSection, IdRow, InfoGrid, InfoItem } from '@/features/logs/detail-ui'
import { LogRequestDetails } from '@/features/logs/LogRequestDetails'
import { LogBillingDetails } from '@/features/logs/LogBillingDetails'
import { billingStatus } from '@/features/logs/types'
import { formatCount, formatMoney } from '@/lib/money'
import { TokenBreakdown } from '@/features/logs/TokenBreakdown'
import type { Snapshot } from '@/features/logs/types'
import { maskEmails } from '@/lib/mask'
import { RefundInline } from './RefundInline'
import type { LogRow } from './types'

/// 详情抽屉：排障字段全集。分区——标识（工单锚点）/ 调度（哪条链路怎么走的）/
/// 金额构成。ratio_snapshot 原样给出，倍率争议时直接对着快照讲。
function parseSnapshot(raw: string): Snapshot | null {
  try { const value = JSON.parse(raw); return value && typeof value === 'object' && typeof value.mode === 'string' ? value as Snapshot : null }
  catch { return null }
}

export function RowDetail({ row }: { row: LogRow }) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const item = (label: string, value: React.ReactNode) => <InfoItem label={label}>{value}</InfoItem>
  const section = (title: string, icon: LucideIcon, children: React.ReactNode) => (
    <DetailSection icon={icon} title={title}><InfoGrid cols={3}>{children}</InfoGrid></DetailSection>
  )
  return (
    <DetailBody>
      <LogErrorDetails failed={row.is_error} code={row.error_code} upstreamStatus={row.upstream_status} diagnostics={row.diagnostics} />
      {/* 实扣：详情里最先要看的数。成本与毛利只在有成本数据时出现；负毛利标红——这一笔在亏钱 */}
      <DetailAmount label={t('logs:final')} value={formatMoney(row.amount_micro, locale)}>
        {(row.upstream_cost_known === true || row.upstream_cost_micro > 0) && <dl className="min-w-32 rounded-lg border border-border/60 bg-card/75 px-3 py-2">
          <dt className="text-xs leading-5 text-muted-foreground">{t('admin:statMargin')}</dt>
          <dd className={`mt-0.5 text-[13px] leading-5 font-medium tabular-nums ${row.amount_micro < row.upstream_cost_micro ? 'text-destructive' : 'text-success'}`}>{formatMoney(row.amount_micro - row.upstream_cost_micro, locale)}</dd>
        </dl>}
      </DetailAmount>
      <dl aria-label={t('admin:logsObjects')} className="grid gap-2.5 sm:grid-cols-3">
        {[
          { label: t('analytics:dimUser'), icon: User, name: row.username, id: row.user_id },
          { label: t('analytics:dimApiKey'), icon: KeyRound, name: row.key_name || (row.key_prefix ? t('flow:unnamed_api_key') : undefined), id: row.api_key_id, prefix: row.key_prefix },
          { label: t('analytics:dimChannel'), icon: Server, name: row.channel_name ? maskEmails(row.channel_name) : (row.channel_id <= 0 ? t('admin:dashboardUnassignedChannel') : undefined), id: row.channel_id },
        ].map((object) => <div data-slot="detail-object" key={object.label} className="min-w-0 space-y-1 rounded-xl border border-border bg-card p-3">
          <dt className="flex items-center gap-1.5 text-xs leading-5 text-muted-foreground"><span aria-hidden className="flex h-5 w-5 shrink-0 items-center justify-center rounded-md bg-primary/8 text-primary"><object.icon className="h-3 w-3" /></span>{object.label}</dt>
          <dd className="break-words text-[13px] leading-5 font-medium [overflow-wrap:anywhere]">{object.name || t('admin:logsNameUnavailable')}</dd>
          <dd className="break-all text-xs leading-5 text-muted-foreground tabular-nums">{object.id > 0 ? `ID ${object.id}` : '—'}{object.prefix ? ` · ${object.prefix}…` : ''}</dd>
        </div>)}
      </dl>
      {section(
        t('admin:logsDetailIdentity'),
        Fingerprint,
        <>
          <div className="col-span-full grid gap-2">
            <IdRow label={t('logs:requestId')} value={row.request_id} />
            {row.upstream_request_id && <IdRow label={t('logs:upstreamId')} value={row.upstream_request_id} />}
          </div>
          {item(t('admin:logsNode'), row.node || '—')}
          {row.client_ip && item('IP', row.client_ip)}
          {item(t('logs:group'), row.group)}
        </>,
      )}
      {section(
        t('admin:logsDetailRouting'),
        Route,
        <>
          {item(t('admin:provider'), row.provider || '—')}
          {item(t('logs:requestedModel'), row.requested_model || t('logs:notRecorded'))}
          {item(t('logs:upstreamModel'), row.upstream_model || t('logs:notRecorded'))}
          {item(t('logs:endpoint'), row.endpoint || t('logs:notRecorded'))}
          {item(t('logs:upstreamEndpoint'), row.upstream_endpoint || t('logs:notRecorded'))}
          {item(t('logs:channelKey'), row.channel_key_id > 0 ? `ID ${row.channel_key_id}` : '—')}
          {item(t('logs:upstreamStatus'), String(row.upstream_status || '—'))}
          {item(t('admin:logsRetries'), String(row.retry_count))}
          {item(t('admin:statFailovers'), String(row.failover_count))}
          {item(t('admin:logsSticky'), row.sticky_layer > 0 ? `L${row.sticky_layer}` : '—')}
          {row.usage.reasoning_tokens > 0 &&
            item(t('admin:logsReasoning'), formatCount(row.usage.reasoning_tokens, locale))}
        </>,
      )}
      {section(
        t('admin:logsDetailBilling'),
        Wallet,
        <>
          {row.status != null && item(t('logs:billingState'), t(`logs:${billingStatus(row.status)}`))}
          {item(t('logs:original'), formatMoney(row.original_amount_micro, locale))}
          {row.discount_micro > 0 &&
            item(t('logs:discount'), `-${formatMoney(row.discount_micro, locale)}`)}
          {(row.upstream_cost_known === true || row.upstream_cost_micro > 0) &&
            item(t('admin:statUpstreamCost'), formatMoney(row.upstream_cost_micro, locale))}
          {row.ratio_snapshot && (
            <InfoItem wide label={t('admin:logsRatioSnapshot')}>
              <code className="block break-all rounded-md bg-muted/50 px-2.5 py-2 font-mono text-xs font-normal">{row.ratio_snapshot}</code>
            </InfoItem>
          )}
        </>,
      )}
      <LogRequestDetails row={row} />
      <LogPerformanceDetails row={row} />
      <TokenBreakdown usage={row.usage} recorded={row.usage_details_recorded} />
      <LogBillingDetails row={{ ...row, pricing_snapshot: parseSnapshot(row.ratio_snapshot) }} status={row.status} />
      <RefundInline row={row} />
    </DetailBody>
  )
}
