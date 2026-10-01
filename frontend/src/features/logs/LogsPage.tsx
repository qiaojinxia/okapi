import { useInfiniteQuery, useQuery, useQueryClient } from '@tanstack/react-query'
import type { InfiniteData } from '@tanstack/react-query'
import { getRouteApi } from '@tanstack/react-router'
import { CircleHelp, Download, FileText, RotateCw, Search } from 'lucide-react'
import { useEffect, useId, useState } from 'react'
import type { ReactNode } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { Alert } from '@/components/ui/alert'
import { DateRangePicker } from '@/components/ui/date-range'
import type { DateRange } from '@/components/ui/date-range'
import { Input } from '@/components/ui/input'
import { PageHeader } from '@/components/ui/page'
import { Pagination } from '@/components/ui/pagination'
import { RowExpander } from '@/components/ui/row-expander'
import { TableSkeleton } from '@/components/ui/skeleton'
import { EmptyState, ErrorState } from '@/components/ui/state'
import { Switch } from '@/components/ui/switch'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { Tooltip } from '@/components/ui/tooltip'
import { UsageScope } from '@/components/usage-scope'
import { PublicModelSearchInput } from '@/features/models/model-input'
import { useUsageScope } from '@/hooks/use-usage-scope'
import { PAGE_SIZES } from '@/hooks/use-pagination'
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
const routeApi = getRouteApi('/portal/logs')
const validKey = (value: string) => value.trim() === '' || (/^\d+$/.test(value) && Number.isSafeInteger(Number(value)) && Number(value) > 0)
const validRequest = (value: string) => value.trim() === '' || /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(value.trim())

function params(f: Filter, before: number | null = null, limit?: number): string {
  const p = new URLSearchParams({ scope: f.scope })
  if (limit !== undefined) p.set('limit', String(limit))
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
  const [limit, setLimit] = useState(PAGE_SIZES[0])
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
  return <LogList key={`${appliedKey}:${limit}`} filter={applied} ready={usageScope.ready} limit={limit} onLimit={setLimit} filters={
    <section aria-label={t('logs:filters')} data-slot="log-filters" className="min-w-0 space-y-2 rounded-xl border border-border bg-card p-2 shadow-card">
      <div className="flex min-w-0 flex-wrap items-center gap-2">
      <UsageScope {...usageScope} scope={applied.scope} onChange={(scope) => commit({ ...applied, scope, keyId: '' })} />
      <PublicModelSearchInput className="w-full sm:w-64" inputClassName="h-11 md:h-9" aria-label={t('pricing:model')}
        value={draft.model} placeholder={t('portal:logsModelHint')} onChange={(model) => setDraft({ ...draft, model })}
        onChoose={(model) => commit({ ...draft, model })} onSubmit={() => commit(draft)} />
      {applied.scope === 'user' && <PortalKeyFilter className="w-full sm:w-56" value={draft.keyId} onChange={(keyId) => setDraft({ ...draft, keyId })} onChoose={(keyId) => commit({ ...draft, keyId })} onSubmit={() => commit(draft)} />}
      <Switch checked={draft.errorsOnly} onChange={(errorsOnly) => commit({ ...draft, errorsOnly })} label={t('admin:logsErrorsOnly')} />
      <Button size="sm" className="ml-auto h-11 md:h-9" disabled={!valid} onClick={() => commit(draft)}><Search className="h-3.5 w-3.5" />{t('common:search')}</Button>
      </div>
      <section aria-label={t('charts:period')} className="flex min-w-0 flex-wrap items-center gap-2 border-t border-border/60 pt-2">
      <DateRangePicker className="md:py-1 md:[&_summary]:min-h-6" today={todayInTimezone(applied.timezone)} value={applied.range} onApply={(range) => commit({ ...draft, range })} />
      {applied.range && <Button size="sm" variant="ghost" onClick={() => commit({ ...applied, range: null })}>{t('portal:logsClearDates')}</Button>}
      <Input className="w-full font-mono sm:w-80" aria-label={t('admin:logsRequestId')} placeholder={t('logs:requestFilterHint')} value={draft.requestId}
        maxLength={36} aria-invalid={!validRequest(draft.requestId)} onChange={(e) => setDraft({ ...draft, requestId: e.target.value })}
        onKeyDown={(e) => { if (e.key === 'Enter' && !e.nativeEvent.isComposing) commit(draft) }} />
      {(applied.requestId || applied.keyId) && <Button size="sm" variant="ghost" onClick={() => commit({ ...applied, requestId: '', keyId: '' })}>{t('logs:clearLookup')}</Button>}
      <p className="min-w-0 text-xs text-muted-foreground sm:ml-auto">{!valid ? t('logs:invalidLookup') : <>{!applied.range && <>{t('logs:allDatesShort')} · </>}{t('logs:displayTimezone', { timezone: applied.timezone })}</>}</p>
      </section>
    </section>
  } />
}

function LogList({ filter, ready, limit, onLimit, filters }: { filter: Filter; ready: boolean; limit: number; onLimit: (limit: number) => void; filters: ReactNode }) {
  const { t, i18n } = useTranslation(), locale = i18n.language
  const client = useQueryClient()
  const [page, setPage] = useState(0)
  const [selected, setSelected] = useState<number | null>(null)
  const detailId = useId(), filterKey = params(filter)
  const queryKey = [...qk.logs(params(filter, null, limit)), 'paged']
  const q = useInfiniteQuery({
    queryKey,
    queryFn: ({ pageParam }) => apiFetch<LogsResp>(`/api/me/logs?${params(filter, pageParam, limit)}`),
    initialPageParam: null as number | null,
    getNextPageParam: (last) => last.data.length < limit ? undefined : last.next_before,
    enabled: ready, retry: false,
  })
  const stats = useQuery({
    queryKey: ['portal-logs-stat', filterKey],
    queryFn: () => apiFetch<LogStats>(`/api/me/logs/stat?${filterKey}`),
    enabled: ready, retry: false, staleTime: 30_000,
  })
  const current = q.data?.pages[page]
  const rows = current?.data ?? []
  const next = q.data?.pages[page + 1]
  const hasMore = next ? next.data.length > 0 : rows.length >= limit && current?.next_before != null
  const changePage = async (offset: number) => {
    if (q.isFetching) return
    const target = Math.floor(offset / limit)
    if (target < 0 || target > page + 1) return
    if (!q.data?.pages[target]) {
      if (!q.hasNextPage) return
      const result = await q.fetchNextPage({ cancelRefetch: false })
      if (result.isError || !result.data?.pages[target]?.data.length) return
    }
    setSelected(null)
    setPage(target)
  }
  const refresh = () => {
    setPage(0)
    setSelected(null)
    // Start a fresh cursor chain, rather than reloading every previously visited page.
    client.setQueryData<InfiniteData<LogsResp>>(queryKey, (old) => old ? { pages: old.pages.slice(0, 1), pageParams: old.pageParams.slice(0, 1) } : old)
    void q.refetch()
    void stats.refetch()
  }
  const showKey = filter.scope === 'user'
  return <div className="list-page [--page-gap:12px]">
    <PageHeader title={t('logs:title')} description={t('portal:logsDesc')} icon={FileText} className="[&_p]:text-xs [&_p]:leading-5" action={<>
          <Button size="sm" variant="outline" disabled={!rows.length || q.isFetching} onClick={() => exportCsv(rows, showKey)} title={t('logs:exportPageHint', { n: rows.length })}>
            <Download className="h-3.5 w-3.5" />{t('logs:exportPage')}
          </Button>
          <Button size="sm" variant="outline" disabled={!ready || q.isFetching} loading={q.isRefetching || stats.isRefetching} onClick={refresh}>
            {!q.isRefetching && <RotateCw className="h-3.5 w-3.5" />}{t('common:refresh')}
          </Button>
    </>} />
    {filters}
    <LogSummary layout="strip" data={stats.data} loading={stats.isPending} error={stats.isError} onRetry={() => void stats.refetch()} />
    {q.isError && current && <Alert tone="destructive" action={<Button size="sm" variant="outline" onClick={() => q.isFetchNextPageError ? void changePage((page + 1) * limit) : refresh()}>{t('common:retry')}</Button>}>{describeError(q.error)}</Alert>}
      {q.isError && !current ? <ErrorState message={describeError(q.error)} onRetry={() => void q.refetch()} /> : q.isPending ? <TableSkeleton dense rows={8} cols={showKey ? 8 : 7} /> : !rows.length ? <EmptyState hint={t('portal:emptyUsageHint')} /> :
        <Table dense stickyHeader scrollResetKey={`${filterKey}:${page}:${limit}`} aria-label={t('logs:title')}>
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
    <Pagination limit={limit} offset={page * limit} hasMore={hasMore} pageSizes={PAGE_SIZES} onLimit={onLimit}
      disabled={!ready || q.isFetching} onOffset={(offset) => void changePage(offset)}
      summary={<span className="inline-flex flex-wrap items-center gap-x-3 gap-y-1">
        <span>{t('common:pageN', { page: page + 1 })}</span>
        <span>{t(stats.data?.records != null ? 'logs:pageRangeTotal' : 'logs:pageRange', { from: rows.length ? page * limit + 1 : 0, to: rows.length ? page * limit + rows.length : 0, total: stats.data?.records })}</span>
      </span>} />
    <LogDetail row={rows.find((row) => row.id === selected) ?? null} onClose={() => setSelected(null)} id={detailId} timezone={filter.timezone} />
  </div>
}

function exportCsv(rows: LogRow[], withKey: boolean) {
  downloadCsv('okapi-usage-page',
    ['time', 'billing_status', ...(withKey ? ['key', 'key_id'] : []), 'model', 'requested_model', 'endpoint', 'stream',
      'prompt_tokens', 'cached_tokens', 'cache_read_reported', 'cache_write_tokens', 'cache_write_reported', 'completion_tokens', 'reasoning_tokens',
      'cache_write_5m_tokens', 'cache_write_1h_tokens', 'audio_prompt_tokens', 'image_prompt_tokens', 'audio_completion_tokens', 'image_completion_tokens',
      'cache_read_audio_tokens', 'cache_read_image_tokens', 'cache_write_audio_tokens', 'cache_write_image_tokens',
      'prompt_source', 'completion_source', 'upstream_prompt_tokens', 'upstream_completion_tokens', 'service_tier',
      'net_amount_usd', 'charged_amount_usd', 'original_usd', 'discount_usd', 'refunded_amount_usd', 'latency_ms', 'ttft_ms', 'error_code', 'request_id'],
    rows.map((row) => [row.created_at, billingStatus(row.status), ...(withKey ? [row.key_name, row.api_key_id] : []), row.model, row.requested_model, row.endpoint, row.is_stream,
      row.usage.prompt_tokens, cacheRead(row), row.usage.cache_read_reported, cacheWrite(row), row.usage.cache_write_reported, row.usage.completion_tokens, row.usage.reasoning_tokens,
      row.usage.cache_write_5m_tokens, row.usage.cache_write_1h_tokens, row.usage.audio_prompt_tokens, row.usage.image_prompt_tokens, row.usage.audio_completion_tokens, row.usage.image_completion_tokens,
      row.usage.cache_read_modalities?.audio_tokens, row.usage.cache_read_modalities?.image_tokens, row.usage.cache_write_modalities?.audio_tokens, row.usage.cache_write_modalities?.image_tokens,
      row.usage.prompt_source, row.usage.completion_source, row.usage.upstream_usage?.prompt_tokens, row.usage.upstream_usage?.completion_tokens, row.pricing_snapshot?.service_tier,
      microToUsd(netAmount(row)), microToUsd(row.amount_micro), microToUsd(row.original_amount_micro), microToUsd(row.discount_micro), microToUsd(row.status === 30 ? row.amount_micro : 0),
      row.latency_ms, row.is_stream ? row.ttft_ms : null, row.error_code, row.request_id]))
}
