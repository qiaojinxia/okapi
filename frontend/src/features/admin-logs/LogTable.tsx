import { useEffect, useId, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Drawer } from '@/components/ui/drawer'
import { LogTokenUsage } from '@/features/logs/LogTokenUsage'
import { LogPerformance } from '@/features/logs/LogPerformance'
import { logMoney } from '@/features/logs/types'
import { Pagination } from '@/components/ui/pagination'
import { RowExpander } from '@/components/ui/row-expander'
import { TableSkeleton } from '@/components/ui/skeleton'
import { EmptyState, ErrorState } from '@/components/ui/state'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import type { Pager } from '@/hooks/use-pagination'
import { describeError } from '@/lib/i18n'
import { maskEmails } from '@/lib/mask'
import { RowDetail } from './RowDetail'
import { useAdminLogs } from './api'
import { toParams } from './search'
import type { Draft } from './search'

export function LogTable({ applied, pager, q }: { applied: Draft; pager: Pager; q: ReturnType<typeof useAdminLogs> }) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const [expanded, setExpanded] = useState<string | null>(null)
  const detailId = useId()
  const params = toParams(applied, pager.offset, pager.limit)
  useEffect(() => { setExpanded(null) }, [params])

  if (q.isError) {
    return <ErrorState message={describeError(q.error)} onRetry={() => void q.refetch()} />
  }
  if (q.isPending) {
    return <TableSkeleton dense rows={10} cols={9} />
  }
  const rows = q.data.data
  return (
    <div className="list-page-section">
      {rows.length === 0 ? (
        <EmptyState hint={t('admin:logsEmptyHint')} />
      ) : (
        <Table dense stickyHeader aria-label={t('admin:logsNav')} scrollResetKey={params} className="min-w-[84rem] table-fixed" wrapperClassName="[container-type:inline-size]">
          <colgroup>
            <col className="w-6" /><col className="w-32" /><col className="w-26" /><col className="w-30" />
            {/* Model and channel share the space left by the fixed metric columns. */}
            <col /><col /><col className="w-78" /><col className="w-28" /><col className="w-64" /><col className="w-16" />
          </colgroup>
          <THead>
            <Tr>
              <Th className="w-6" />
              <Th>{t('logs:time')}</Th>
              <Th>{t('common:status')}</Th>
              <Th>{t('admin:logsUser')}</Th>
              <Th>{t('pricing:model')}</Th>
              <Th>{t('admin:logsChannel')}</Th>
              <Th>{t('logs:tokenUsage')}</Th>
              <Th numeric>{t('common:amount')}</Th>
              <Th numeric>{t('logs:performance')}</Th>
              <Th>{t('admin:logsClient')}</Th>
            </Tr>
          </THead>
          <TBody>
            {rows.map((r) => {
              const open = expanded === r.request_id
              return (
                <Tr key={r.request_id}
                  className="cursor-pointer"
                  selected={open}
                  aria-expanded={open}
                  onClick={() => setExpanded(open ? null : r.request_id)}
                >
                  <Td className="px-1 text-muted-foreground">
                    <RowExpander open={open} name={r.request_id} controls={detailId} onToggle={() => setExpanded(open ? null : r.request_id)} />
                  </Td>
                  <Td className="whitespace-nowrap font-mono text-xs text-muted-foreground">{new Date(r.ts).toLocaleString(locale, { timeZone: 'UTC', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', second: '2-digit', hourCycle: 'h23' })}</Td>
                  <Td className="py-1">
                    <div className="flex min-w-0 flex-col items-start leading-tight">
                      <Badge dot variant={r.is_error ? 'destructive' : 'success'} className={r.error_code ? 'min-h-5 py-0' : undefined}>
                        {t(r.is_error ? 'logs:failed' : 'logs:ok')}
                      </Badge>
                      {r.error_code && <span className="max-w-full truncate font-mono text-[11px] leading-[14px] text-muted-foreground" title={r.error_code}>{r.error_code}</span>}
                    </div>
                  </Td>
                  <Td className="truncate text-xs" title={r.username || undefined}>
                    {r.username || `ID ${r.user_id}`}
                  </Td>
                  <Td className="truncate font-mono text-xs" title={r.model}>{r.model}</Td>
                  <Td className="truncate text-xs" title={r.channel_name ? maskEmails(r.channel_name) : undefined}>
                    {r.channel_name ? maskEmails(r.channel_name) : (r.channel_id > 0 ? `ID ${r.channel_id}` : t('admin:dashboardUnassignedChannel'))}
                  </Td>
                  <Td className="py-1"><LogTokenUsage row={r} /></Td>
                  <Td numeric className="whitespace-nowrap font-medium">{logMoney(r.amount_micro, locale)}</Td>
                  <Td numeric className="py-1"><LogPerformance row={r} /></Td>
                  <Td className="truncate text-xs text-muted-foreground" title={r.client_type || undefined}>{r.client_type || '—'}</Td>
                </Tr>

              )
            })}
          </TBody>
        </Table>
      )}
      {/* CH 明细无 total 计数，按“整页 = 可能有下一页”翻。 */}
      <Pagination {...pager} hasMore={rows.length >= pager.limit} disabled={q.isFetching}
        summary={<span className="inline-flex flex-wrap items-center gap-x-3 gap-y-1"><span>{t('common:pageN', { page: Math.floor(pager.offset / pager.limit) + 1 })}</span><span>{t('logs:pageRange', { from: rows.length ? pager.offset + 1 : 0, to: rows.length ? pager.offset + rows.length : 0 })}</span></span>} />
      <Drawer open={rows.some((row) => row.request_id === expanded)} onClose={() => setExpanded(null)} title={t('logs:detailTitle')} description={t('logs:detailHint')} size="lg">
        <div id={detailId}>{rows.filter((row) => row.request_id === expanded).map((row) => <RowDetail key={row.request_id} row={row} />)}</div>
      </Drawer>
    </div>
  )
}
