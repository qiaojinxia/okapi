import { useInfiniteQuery, useQuery } from '@tanstack/react-query'
import { getRouteApi } from '@tanstack/react-router'
import { CircleHelp, Download, FileText, RotateCw, Search } from 'lucide-react'
import { useEffect, useId, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { DateRangePicker } from '@/components/ui/date-range'
import type { DateRange } from '@/components/ui/date-range'
import { Input } from '@/components/ui/input'
import { PageHeader, Toolbar } from '@/components/ui/page'
import { RowExpander } from '@/components/ui/row-expander'
import { TableSkeleton } from '@/components/ui/skeleton'
import { EmptyState, ErrorState } from '@/components/ui/state'
import { Switch } from '@/components/ui/switch'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { Tooltip } from '@/components/ui/tooltip'
import { UsageScope } from '@/components/usage-scope'
import { PublicModelSearchInput } from '@/features/models/model-input'
import { useUsageScope } from '@/hooks/use-usage-scope'
import { apiFetch } from '@/lib/api'
import { todayInTimezone } from '@/lib/calendar-range'
import { downloadCsv, microToUsd } from '@/lib/csv'
import { describeError } from '@/lib/i18n'
import { qk } from '@/lib/query-keys'
import { LogDetail, LogStatus } from './LogDetail'
import { LogSummary } from './LogSummary'
import { LogTokenUsage } from './LogTokenUsage'
import { PortalKeyFilter } from './PortalKeyFilter'
import { billingStatus, cacheRead, cacheWrite, duration, logMoney, netAmount } from './types'
import type { LogRow, LogsResp, LogStats } from './types'

interface Filter {
  scope: 'key' | 'user'
  model: string
  errorsOnly: boolean
  keyId: string
  requestId: string
  range: DateRange | null
  timezone: string
}
const PAGE = 50
const routeApi = getRouteApi('/portal/logs')
const validKey = (value: string) => value.trim() === '' || (/^\d+$/.test(value) && Number.isSafeInteger(Number(value)) && Number(value) > 0)
const validRequest = (value: string) => value.trim() === '' || /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(value.trim())

function params(f: Filter, before: number | null): string {
  const p = new URLSearchParams({ limit: String(PAGE), scope: f.scope })
  if (f.model.trim()) p.set('model', f.model.trim())
  if (f.errorsOnly) p.set('errors_only', 'true')
  if (f.scope === 'user' && f.keyId) p.set('api_key_id', f.keyId)
  if (f.requestId.trim()) p.set('request_id', f.requestId.trim())
  if (f.range) {
    p.set('start_date', f.range.start)
    p.set('end_date', f.range.end)
    p.set('timezone', f.timezone)
  }
  if (before !== null) p.set('before', String(before))
  return p.toString()
}

export function LogsPage() {
  const { t } = useTranslation()
  const search = routeApi.useSearch(), navigate = routeApi.useNavigate()
  const usageScope = useUsageScope(search.scope)
  const applied: Filter = {
    scope: usageScope.scope, model: search.model ?? '', errorsOnly: search.errors_only === true,
    keyId: usageScope.scope === 'user' && search.api_key_id ? String(search.api_key_id) : '',
    requestId: search.request_id ?? '',
    range: search.start_date && search.end_date ? { start: search.start_date, end: search.end_date } : null,
    timezone: search.timezone ?? 'UTC',
  }
  const [draft, setDraft] = useState<Filter>(applied)
  const appliedKey = JSON.stringify(applied)
  useEffect(() => { setDraft(JSON.parse(appliedKey) as Filter) }, [appliedKey])
  const valid = validKey(draft.keyId) && validRequest(draft.requestId)
  const commit = (next: Filter) => {
    if (!validKey(next.keyId) || !validRequest(next.requestId)) return
    usageScope.setScope(next.scope)
    setDraft(next)
    void navigate({ search: {
      scope: next.scope, model: next.model.trim() || undefined, errors_only: next.errorsOnly ? true : undefined,
      api_key_id: next.scope === 'user' && next.keyId ? Number(next.keyId) : undefined,
      request_id: next.requestId.trim() || undefined,
      start_date: next.range?.start, end_date: next.range?.end, timezone: next.range ? next.timezone : undefined,
    } })
  }
  return <div className="list-page">
    <PageHeader title={t('logs:title')} description={t('portal:logsDesc')} icon={FileText} />
    <Toolbar className="max-sm:[&>div:first-child]:basis-full max-sm:[&>div:last-child]:w-full max-sm:[&>div:last-child]:justify-end" filters={<>
      <UsageScope {...usageScope} scope={applied.scope} onChange={(scope) => commit({ ...applied, scope, keyId: '' })} />
      <PublicModelSearchInput className="w-full sm:w-80" inputClassName="h-11 md:h-9" aria-label={t('pricing:model')}
        value={draft.model} placeholder={t('portal:logsModelHint')} onChange={(model) => setDraft({ ...draft, model })}
        onChoose={(model) => commit({ ...draft, model })} onSubmit={() => commit(draft)} />
      <Switch checked={draft.errorsOnly} onChange={(errorsOnly) => commit({ ...applied, errorsOnly })} label={t('admin:logsErrorsOnly')} />
    </>} selection={<Button size="sm" disabled={!valid} onClick={() => commit(draft)}><Search className="h-3.5 w-3.5" />{t('common:search')}</Button>} />
    <section aria-label={t('charts:period')} className="flex min-w-0 flex-wrap items-center gap-2 rounded-xl border border-border bg-card px-3 py-2">
      <DateRangePicker today={todayInTimezone(applied.timezone)} value={applied.range} onApply={(range) => commit({ ...applied, range })} />
      {applied.range && <Button size="sm" variant="ghost" onClick={() => commit({ ...applied, range: null })}>{t('portal:logsClearDates')}</Button>}
      {applied.scope === 'user' && <PortalKeyFilter value={draft.keyId} onChange={(keyId) => setDraft({ ...draft, keyId })} onChoose={(keyId) => commit({ ...draft, keyId })} onSubmit={() => commit(draft)} />}
      <Input className="w-full font-mono sm:w-96" aria-label={t('admin:logsRequestId')} placeholder={t('logs:requestFilterHint')} value={draft.requestId}
        maxLength={36} aria-invalid={!validRequest(draft.requestId)} onChange={(e) => setDraft({ ...draft, requestId: e.target.value })}
        onKeyDown={(e) => { if (e.key === 'Enter' && !e.nativeEvent.isComposing) commit(draft) }} />
      {(applied.requestId || applied.keyId) && <Button size="sm" variant="ghost" onClick={() => commit({ ...applied, requestId: '', keyId: '' })}>{t('logs:clearLookup')}</Button>}
      <p className="min-w-0 basis-full break-words text-xs text-muted-foreground">{!valid ? t('logs:invalidLookup') : <>{applied.range ? t('portal:logsDateTimezone', { timezone: applied.timezone }) : t('portal:logsAllDates')} · {t('logs:displayTimezone', { timezone: applied.timezone })}</>}</p>
    </section>
    {usageScope.ready ? <LogList filter={applied} /> : <TableSkeleton dense rows={8} cols={applied.scope === 'user' ? 8 : 7} />}
  </div>
}

function LogList({ filter }: { filter: Filter }) {
  const { t, i18n } = useTranslation(), locale = i18n.language
  const [selected, setSelected] = useState<number | null>(null)
  const detailId = useId(), filterKey = params(filter, null)
  useEffect(() => { setSelected(null) }, [filterKey])
  const q = useInfiniteQuery({
    queryKey: qk.logs(filterKey),
    queryFn: ({ pageParam }) => apiFetch<LogsResp>(`/api/me/logs?${params(filter, pageParam)}`),
    initialPageParam: null as number | null,
    getNextPageParam: (last) => last.next_before,
  })
  const stats = useQuery({
    queryKey: ['portal-logs-stat', filterKey],
    queryFn: () => apiFetch<LogStats>(`/api/me/logs/stat?${filterKey}`),
    retry: false,
  })
  const rows = q.data?.pages.flatMap((p) => p.data) ?? []
  const showKey = filter.scope === 'user'
  return <>
    <LogSummary data={stats.data} loading={stats.isPending} error={stats.isError} onRetry={() => void stats.refetch()} />
    <div className="list-page-section">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <span className="text-xs text-muted-foreground">{stats.data?.records != null ? t('logs:loadedOf', { n: rows.length, total: stats.data.records }) : t('portal:logsLoaded', { n: rows.length })}</span>
        <div className="flex gap-2">
          <Button size="sm" variant="outline" disabled={!rows.length} onClick={() => exportCsv(rows, showKey)} title={t('logs:exportHint', { n: rows.length })}>
            <Download className="h-3.5 w-3.5" />{t('logs:exportLoaded')}
          </Button>
          <Button size="sm" variant="outline" loading={q.isRefetching || stats.isRefetching} onClick={() => { void q.refetch(); void stats.refetch() }}>
            {!q.isRefetching && <RotateCw className="h-3.5 w-3.5" />}{t('common:refresh')}
          </Button>
        </div>
      </div>
      {q.isError ? <ErrorState message={describeError(q.error)} onRetry={() => void q.refetch()} /> : q.isPending ? <TableSkeleton dense rows={8} cols={showKey ? 8 : 7} /> : !rows.length ? <EmptyState hint={t('portal:emptyUsageHint')} /> :
        <Table dense stickyHeader scrollResetKey={filterKey} aria-label={t('logs:title')}>
          <THead><Tr><Th className="w-6" /><Th>{t('logs:time')}</Th><Th>{t('logs:billingState')}</Th>{showKey && <Th>{t('portal:keys')}</Th>}
            <Th>{t('pricing:model')}</Th><Th className="w-px">
              <Tooltip content={t('logs:tokenUsageHint')}>
                <button type="button" aria-label={t('logs:tokenUsageHelp')} className="inline-flex h-6 items-center gap-1.5 rounded outline-none hover:text-foreground focus-visible:ring-2 focus-visible:ring-primary/40">
                  {t('logs:tokenUsage')}<CircleHelp aria-hidden className="h-3.5 w-3.5" />
                </button>
              </Tooltip>
            </Th><Th numeric>{t('logs:netSpend')}</Th><Th numeric>{t('logs:performance')}</Th>
          </Tr></THead>
          <TBody>{rows.map((row) => <Tr key={row.id} className="cursor-pointer" selected={selected === row.id} onClick={() => setSelected(row.id)}>
            <Td className="px-1"><RowExpander open={selected === row.id} name={row.request_id} controls={detailId} onToggle={() => setSelected(row.id)} /></Td>
            <Td className="whitespace-nowrap tabular-nums text-muted-foreground" title={new Date(row.created_at).toLocaleString(locale, { timeZone: filter.timezone })}>{new Date(row.created_at).toLocaleString(locale, { timeZone: filter.timezone, month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', second: '2-digit', hourCycle: 'h23' })}</Td>
            <Td><LogStatus row={row} /></Td>
            {showKey && <Td className="max-w-28 truncate" title={row.key_name || undefined}>{row.key_name || (row.api_key_id !== null ? `#${row.api_key_id}` : '—')}</Td>}
            <Td className="max-w-52 truncate font-mono" title={row.model}>{row.model}</Td>
            <Td className="py-1"><LogTokenUsage row={row} /></Td>
            <Td numeric className="font-medium">{logMoney(netAmount(row), locale)}</Td>
            <Td numeric className="py-1"><div className="text-xs leading-4"><span className="mr-2 text-muted-foreground">{t('logs:ttft')}</span>{row.is_stream ? duration(row.ttft_ms, locale) : t('logs:nonStreaming')}</div><div className="text-xs leading-4"><span className="mr-2 text-muted-foreground">{t('logs:totalShort')}</span>{duration(row.latency_ms, locale)}</div></Td>
          </Tr>)}</TBody>
        </Table>}
      {q.hasNextPage && <Button variant="outline" className="self-center" disabled={q.isFetchingNextPage} onClick={() => void q.fetchNextPage()}>{t(q.isFetchingNextPage ? 'common:loading' : 'portal:logsLoadMore')}</Button>}
    </div>
    <LogDetail row={rows.find((row) => row.id === selected) ?? null} onClose={() => setSelected(null)} id={detailId} timezone={filter.timezone} />
  </>
}

function exportCsv(rows: LogRow[], withKey: boolean) {
  downloadCsv('okapi-usage-loaded',
    ['time', 'billing_status', ...(withKey ? ['key', 'key_id'] : []), 'model', 'requested_model', 'endpoint', 'stream',
      'prompt_tokens', 'cached_tokens', 'cache_read_reported', 'cache_write_tokens', 'cache_write_reported', 'completion_tokens', 'reasoning_tokens',
      'net_amount_usd', 'charged_amount_usd', 'original_usd', 'discount_usd', 'refunded_amount_usd', 'latency_ms', 'ttft_ms', 'error_code', 'request_id'],
    rows.map((row) => [row.created_at, billingStatus(row.status), ...(withKey ? [row.key_name, row.api_key_id] : []), row.model, row.requested_model, row.endpoint, row.is_stream,
      row.usage.prompt_tokens, cacheRead(row), row.usage.cache_read_reported, cacheWrite(row), row.usage.cache_write_reported, row.usage.completion_tokens, row.usage.reasoning_tokens,
      microToUsd(netAmount(row)), microToUsd(row.amount_micro), microToUsd(row.original_amount_micro), microToUsd(row.discount_micro), microToUsd(row.status === 30 ? row.amount_micro : 0),
      row.latency_ms, row.is_stream ? row.ttft_ms : null, row.error_code, row.request_id]))
}
