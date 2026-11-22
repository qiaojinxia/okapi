import { useInfiniteQuery, useQuery } from '@tanstack/react-query'
import { getRouteApi, useNavigate } from '@tanstack/react-router'
import dayjs from 'dayjs'
import { Download, Filter, RotateCcw } from 'lucide-react'
import { useEffect, useId, useMemo, useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { AuditSearch } from '@/routes/admin.audit'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { CopyButton } from '@/components/ui/copy-button'
import { Drawer, FieldGroup } from '@/components/ui/drawer'
import { Input } from '@/components/ui/input'
import { PageHeader, Toolbar } from '@/components/ui/page'
import { RowExpander } from '@/components/ui/row-expander'
import { Segmented } from '@/components/ui/segmented'
import { Select } from '@/components/ui/select'
import { TableSkeleton } from '@/components/ui/skeleton'
import { EmptyState, ErrorState } from '@/components/ui/state'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { DEFAULT_PAGE_SIZE } from '@/hooks/use-pagination'
import { ApiError, apiFetch, getKey } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { qk } from '@/lib/query-keys'

const routeApi = getRouteApi('/admin/audit')
const DEFAULT_HOURS = 168
const HOURS = [24, 168, 720, 2160] as const
const PAGE = DEFAULT_PAGE_SIZE

const BUILTIN_ACTION_PREFIXES = [
  'channel.',
  'pricing.',
  'user.',
  'apikey.',
  'role.',
  'billing.',
  'settings.',
  'auth.',
  'mcp.',
  'plan.',
  'redemption.',
  'payment.',
  'subscription.',
  'team.',
  'proxy.',
  'proxy_group.',
  'margin_breaker.',
  'conversation_archive.',
  'notification.',
  'system.',
  'audit.',
  'api.',
] as const

interface AuditRow {
  id: number
  actor: string
  actor_info: { kind: string; id: number | null; label: string | null } | null
  action: string
  target: string | null
  detail: Record<string, unknown> | null
  ip: string | null
  user_agent: string | null
  trace_id: string | null
  actor_key_id: number | null
  created_at: string
}

interface AuditResp {
  data: AuditRow[]
  has_more: boolean
  next_before: number | null
}

interface Draft {
  actor: string
  action: string
  target: string
  ip: string
  q: string
  trace_id: string
  hours: number
  from: string
  to: string
}

function isoToLocalInput(iso?: string): string {
  if (!iso) return ''
  const d = dayjs(iso)
  return d.isValid() ? d.format('YYYY-MM-DDTHH:mm') : ''
}

function localInputToIso(local: string): string | undefined {
  const s = local.trim()
  if (!s) return undefined
  const d = dayjs(s)
  return d.isValid() ? d.toISOString() : undefined
}

function fromSearch(s: AuditSearch): Draft {
  return {
    actor: s.actor ?? '',
    action: s.action ?? '',
    target: s.target ?? '',
    ip: s.ip ?? '',
    q: s.q ?? '',
    trace_id: s.trace_id ?? '',
    hours: s.hours ?? DEFAULT_HOURS,
    from: isoToLocalInput(s.from),
    to: isoToLocalInput(s.to),
  }
}

function toSearch(d: Draft): AuditSearch {
  const from = localInputToIso(d.from)
  const to = localInputToIso(d.to)
  return {
    actor: d.actor.trim() || undefined,
    action: d.action.trim() || undefined,
    target: d.target.trim() || undefined,
    ip: d.ip.trim() || undefined,
    q: d.q.trim() || undefined,
    trace_id: d.trace_id.trim() || undefined,
    from,
    to,
    hours: from || d.hours === DEFAULT_HOURS ? undefined : d.hours,
  }
}

function buildFilterParams(s: AuditSearch, extra?: Record<string, string>): string {
  const p = new URLSearchParams()
  if (s.from) p.set('from', s.from)
  else p.set('hours', String(s.hours ?? DEFAULT_HOURS))
  if (s.to) p.set('to', s.to)
  if (s.actor) p.set('actor', s.actor)
  if (s.action) p.set('action', s.action)
  if (s.target) p.set('target', s.target)
  if (s.ip) p.set('ip', s.ip)
  if (s.q) p.set('q', s.q)
  if (s.trace_id) p.set('trace_id', s.trace_id)
  if (extra) {
    for (const [k, v] of Object.entries(extra)) p.set(k, v)
  }
  return p.toString()
}

function toParams(s: AuditSearch, before?: number): string {
  const extra: Record<string, string> = { limit: String(PAGE) }
  if (before !== undefined) extra.before = String(before)
  return buildFilterParams(s, extra)
}

/// 动作着色：删除 / 丢弃 / 停用 / 拒绝 / 封禁为高危红，失败 / 熔断 / 复制 / 退款为警示黄。
function actionTone(action: string): 'destructive' | 'warning' | 'muted' | 'default' {
  const verb = action.split('.').pop() ?? ''
  if (/^(delete|discard|revoke|ban)|denied/.test(verb)) return 'destructive'
  if (/failed|disable|refund|flush|trip|copy|stop/.test(verb)) return 'warning'
  if (/^(create|upsert|publish|credit|set_|rotate|enable|settled|claim|setup)/.test(verb)) return 'default'
  return 'muted'
}

function isPlainObject(v: unknown): v is Record<string, unknown> {
  return typeof v === 'object' && v !== null && !Array.isArray(v)
}

function formatJsonValue(v: unknown, pretty = false): string {
  if (v === null || v === undefined) return 'null'
  if (typeof v === 'object') return JSON.stringify(v, null, pretty ? 2 : undefined)
  return String(v)
}

function stableEqual(a: unknown, b: unknown): boolean {
  return JSON.stringify(a ?? null) === JSON.stringify(b ?? null)
}

/// detail 按键展示；列表跳过技术上下文与已独立展示的 diff/快照字段。
function detailEntries(
  detail: Record<string, unknown> | null,
  pretty = false,
  skipKeys?: Set<string>,
): [string, string][] {
  if (!detail) return []
  return Object.entries(detail)
    .filter(([k, v]) => !skipKeys?.has(k) && v !== null && v !== undefined && v !== '')
    .map(([k, v]) => [k, formatJsonValue(v, pretty)])
}

function rowIp(r: AuditRow): string | null {
  return r.ip ?? (typeof r.detail?.ip === 'string' ? r.detail.ip : null)
}

function rowUa(r: AuditRow): string | null {
  return r.user_agent ?? (typeof r.detail?.ua === 'string' ? r.detail.ua : null)
}

/// 审计日志页：谁在何时从哪改了什么（含变更对比、请求上下文、IP/关键词/自定义区间筛选与导出）。
export function AuditPage() {
  const { t } = useTranslation()
  const search = routeApi.useSearch()
  const navigate = useNavigate({ from: '/admin/audit' })
  const [draft, setDraft] = useState<Draft>(() => fromSearch(search))
  const [selected, setSelected] = useState<number | null>(null)
  const [exporting, setExporting] = useState<'csv' | 'json' | null>(null)
  const [exportNotice, setExportNotice] = useState<{ kind: 'info' | 'error'; text: string } | null>(null)
  const detailId = useId(), params = toParams(search)
  useEffect(() => setDraft(fromSearch(search)), [search])
  useEffect(() => setSelected(null), [params])

  const submit = (d: Draft) => {
    setExportNotice(null)
    void navigate({ search: toSearch(d) })
  }

  const resetAll = () => {
    const empty: Draft = {
      actor: '', action: '', target: '', ip: '', q: '', trace_id: '',
      hours: DEFAULT_HOURS, from: '', to: '',
    }
    setDraft(empty)
    submit(empty)
  }

  const applyQuickFilter = (patch: Partial<Draft>) => {
    const next = { ...fromSearch(search), ...patch }
    setDraft(next)
    setSelected(null)
    submit(next)
  }

  const actions = useQuery({
    queryKey: qk.auditActions,
    queryFn: () => apiFetch<{ data: string[] }>('/admin/audit/actions'),
    staleTime: 300_000,
  })
  const q = useInfiniteQuery({
    queryKey: qk.audit(params),
    queryFn: ({ pageParam }) =>
      apiFetch<AuditResp>(`/admin/audit?${toParams(search, pageParam as number | undefined)}`),
    initialPageParam: undefined as number | undefined,
    getNextPageParam: (last) => (last.has_more ? (last.next_before ?? undefined) : undefined),
  })
  const rows = q.data?.pages.flatMap((p) => p.data) ?? []
  const selectedRow = q.isError ? null : rows.find((row) => row.id === selected) ?? null

  const actionOptions = useMemo(() => {
    const seen = new Set<string>(BUILTIN_ACTION_PREFIXES)
    const rawActions = actions.data?.data ?? []
    for (const a of rawActions) {
      const dot = a.indexOf('.')
      if (dot > 0) seen.add(`${a.slice(0, dot + 1)}`)
    }
    const groups = Array.from(seen).map((p) => ({
      value: p,
      label: t('admin:auditActionGroup', { prefix: p }),
    }))
    return [...groups, ...rawActions.map((a) => ({ value: a, label: a }))]
  }, [actions.data?.data, t])

  const triggerExport = async (format: 'csv' | 'json') => {
    setExporting(format)
    setExportNotice(null)
    try {
      const qs = buildFilterParams(search, { format })
      const key = getKey()
      const headers: Record<string, string> = {}
      if (key) headers.Authorization = `Bearer ${key}`
      const resp = await fetch(`/admin/audit/export?${qs}`, {
        credentials: 'same-origin',
        headers,
      })
      if (!resp.ok) {
        let code = `http_${resp.status}`
        let param: string | undefined
        try {
          const body = (await resp.json()) as { error?: { code?: string; param?: string } }
          code = body.error?.code ?? code
          param = body.error?.param
        } catch {
          // ignore
        }
        throw new ApiError(resp.status, code, param)
      }
      const count = Number(resp.headers.get('x-audit-export-count') ?? '0')
      const truncated = resp.headers.get('x-audit-export-truncated') === 'true'
      const blob = await resp.blob()
      const url = URL.createObjectURL(blob)
      const a = document.createElement('a')
      a.href = url
      a.download = `audit-logs-${dayjs().format('YYYYMMDD-HHmm')}.${format}`
      a.click()
      URL.revokeObjectURL(url)
      if (truncated) {
        setExportNotice({ kind: 'info', text: t('admin:auditExportTruncated', { n: count }) })
      }
    } catch (err) {
      setExportNotice({ kind: 'error', text: describeError(err) })
    } finally {
      setExporting(null)
    }
  }

  const actorLabel = (r: AuditRow) => {
    if (r.actor === 'anon') return t('admin:auditActorAnon')
    const label = r.actor_info?.label
    return label ? `${label}` : r.actor
  }
  const actorKind = (r: AuditRow) => {
    switch (r.actor_info?.kind) {
      case 'admin':
        return t('admin:auditKindAdmin')
      case 'mcp':
        return t('admin:auditKindMcp')
      case 'user':
        return t('admin:auditKindUser')
      case 'system':
        return t('admin:auditKindSystem')
      case 'worker':
        return t('admin:auditKindWorker')
      default:
        return ''
    }
  }
  const hoursLabel = (h: number) =>
    h < 168 ? t('admin:auditHours', { n: h }) : t('admin:lastDays', { days: h / 24 })

  const hasAnyFilter = Boolean(
    search.actor || search.action || search.target || search.ip || search.q || search.trace_id || search.from || search.to || (search.hours && search.hours !== DEFAULT_HOURS),
  )

  const summarizeDetail = (r: AuditRow): string => {
    const d = r.detail
    if (!d) return '—'
    if (isPlainObject(d.before) && isPlainObject(d.after)) {
      const keys = Object.keys(d.after)
      if (keys.length > 0) {
        const preview = keys.slice(0, 3).join(', ')
        const extra = keys.length > 3 ? ` +${keys.length - 3}` : ''
        return t('admin:auditDiffChanged', { fields: `${preview}${extra}` })
      }
    }
    const skip = new Set(['ip', 'ua', 'before', 'after', 'value'])
    const entries = detailEntries(d, false, skip)
    if (entries.length === 0) {
      const fallback = detailEntries(d, false, new Set(['ip', 'ua']))
      if (fallback.length === 0) return '—'
      return `${fallback.slice(0, 2).map(([k, v]) => `${k}=${v}`).join(' · ')}${fallback.length > 2 ? ` · +${fallback.length - 2}` : ''}`
    }
    return `${entries.slice(0, 2).map(([k, v]) => `${k}=${v}`).join(' · ')}${entries.length > 2 ? ` · +${entries.length - 2}` : ''}`
  }

  return (
    <div className="list-page">
      <PageHeader
        title={t('admin:auditTitle')}
        description={t('admin:auditDesc')}
        meta={q.data && t('admin:auditLoaded', { n: rows.length })}
        action={
          <div className="flex flex-wrap items-center gap-2">
            <Button
              size="sm"
              variant="outline"
              disabled={exporting !== null}
              onClick={() => void triggerExport('csv')}
            >
              <Download className="mr-1.5 h-3.5 w-3.5" />
              {exporting === 'csv' ? t('admin:auditExporting') : t('admin:auditExportCsv')}
            </Button>
            <Button
              size="sm"
              variant="outline"
              disabled={exporting !== null}
              onClick={() => void triggerExport('json')}
            >
              <Download className="mr-1.5 h-3.5 w-3.5" />
              {exporting === 'json' ? t('admin:auditExporting') : t('admin:auditExportJson')}
            </Button>
          </div>
        }
      />

      <Toolbar
        filters={
          <div className="flex w-full flex-col gap-2">
            <div className="flex flex-wrap items-center gap-2">
              <Select
                id="au-action"
                className="w-full sm:w-48"
                aria-label={t('admin:auditAction')}
                value={draft.action}
                onChange={(v) => setDraft((d) => ({ ...d, action: v }))}
                placeholder={t('admin:auditActionAll')}
                options={actionOptions}
              />
              <Input
                id="au-target"
                className="w-full sm:w-52"
                aria-label={t('admin:auditTarget')}
                value={draft.target}
                placeholder={t('admin:auditTargetPlaceholder')}
                onChange={(e) => setDraft((d) => ({ ...d, target: e.target.value }))}
                onKeyDown={(e) => { if (e.key === 'Enter') submit(draft) }}
              />
              <Input
                id="au-actor"
                className="w-full font-mono sm:w-40"
                aria-label={t('admin:auditActor')}
                value={draft.actor}
                placeholder={t('admin:auditActorPlaceholder')}
                onChange={(e) => setDraft((d) => ({ ...d, actor: e.target.value }))}
                onKeyDown={(e) => { if (e.key === 'Enter') submit(draft) }}
              />
              <Segmented
                options={HOURS.map((h) => ({ value: h, label: hoursLabel(h) }))}
                value={draft.from ? -1 : draft.hours}
                onChange={(h) => submit({ ...draft, hours: h, from: '', to: '' })}
                size="field"
                ariaLabel={t('admin:logsRange')}
              />
              <Button size="sm" variant="outline" onClick={() => submit(draft)}>
                {t('common:search')}
              </Button>
              {hasAnyFilter && (
                <Button size="sm" variant="ghost" onClick={resetAll} title={t('admin:auditResetFilters')}>
                  <RotateCcw className="mr-1 h-3.5 w-3.5" />
                  {t('admin:auditResetFilters')}
                </Button>
              )}
            </div>
            <div className="flex flex-wrap items-center gap-2">
              <Input
                id="au-keyword"
                className="w-full sm:w-56"
                aria-label={t('admin:auditKeywordPlaceholder')}
                value={draft.q}
                placeholder={t('admin:auditKeywordPlaceholder')}
                onChange={(e) => setDraft((d) => ({ ...d, q: e.target.value }))}
                onKeyDown={(e) => { if (e.key === 'Enter') submit(draft) }}
              />
              <Input
                id="au-ip"
                className="w-full font-mono sm:w-48"
                aria-label="IP"
                value={draft.ip}
                placeholder={t('admin:auditIpPlaceholder')}
                onChange={(e) => setDraft((d) => ({ ...d, ip: e.target.value }))}
                onKeyDown={(e) => { if (e.key === 'Enter') submit(draft) }}
              />
              <Input
                id="au-trace"
                className="w-full font-mono sm:w-48"
                aria-label={t('admin:auditTraceId')}
                value={draft.trace_id}
                placeholder={t('admin:auditTracePlaceholder')}
                onChange={(e) => setDraft((d) => ({ ...d, trace_id: e.target.value }))}
                onKeyDown={(e) => { if (e.key === 'Enter') submit(draft) }}
              />
              <label className="flex items-center gap-1.5 text-xs text-muted-foreground">
                <span className="whitespace-nowrap">{t('admin:auditFrom')}</span>
                <Input
                  id="au-from"
                  type="datetime-local"
                  className="w-44 font-mono text-xs"
                  value={draft.from}
                  onChange={(e) => setDraft((d) => ({ ...d, from: e.target.value }))}
                />
              </label>
              <label className="flex items-center gap-1.5 text-xs text-muted-foreground">
                <span className="whitespace-nowrap">{t('admin:auditTo')}</span>
                <Input
                  id="au-to"
                  type="datetime-local"
                  className="w-44 font-mono text-xs"
                  value={draft.to}
                  onChange={(e) => setDraft((d) => ({ ...d, to: e.target.value }))}
                />
              </label>
            </div>
          </div>
        }
      />

      {exportNotice && (
        <div
          className={
            exportNotice.kind === 'error'
              ? 'rounded-lg border border-destructive/40 bg-destructive/10 px-3 py-2 text-xs text-destructive'
              : 'rounded-lg border border-border bg-muted/40 px-3 py-2 text-xs text-muted-foreground'
          }
        >
          {exportNotice.text}
        </div>
      )}

      {q.isError ? (
        <ErrorState message={describeError(q.error)} onRetry={() => void q.refetch()} />
      ) : q.isPending ? (
        <TableSkeleton rows={8} cols={7} dense />
      ) : rows.length === 0 ? (
        <EmptyState hint={t('admin:auditEmptyHint')} />
      ) : (
        <Table dense stickyHeader aria-label={t('admin:auditTitle')}>
          <THead>
            <Tr>
              <Th className="w-6" />
              <Th>{t('admin:auditTime')}</Th>
              <Th>{t('admin:auditActor')}</Th>
              <Th>{t('admin:auditAction')}</Th>
              <Th>{t('admin:auditTarget')}</Th>
              <Th>{t('admin:auditDetail')}</Th>
              <Th>IP</Th>
            </Tr>
          </THead>
          <TBody>
            {rows.map((r) => {
              const expanded = selected === r.id
              const ip = rowIp(r)
              return (
                <Tr
                  key={r.id}
                  className="cursor-pointer"
                  selected={expanded}
                  aria-expanded={expanded}
                  onClick={() => setSelected(r.id)}
                >
                  <Td className="px-1 text-muted-foreground">
                    <RowExpander open={expanded} name={String(r.id)} controls={detailId} onToggle={() => setSelected(r.id)} />
                  </Td>
                  <Td className="whitespace-nowrap font-mono text-xs text-muted-foreground tabular-nums">
                    {dayjs(r.created_at).format('MM-DD HH:mm:ss')}
                  </Td>
                  <Td>
                    <div className="flex flex-col leading-tight">
                      <span className="max-w-40 truncate">{actorLabel(r)}</span>
                      <span className="font-mono text-[11px] text-muted-foreground">
                        {actorKind(r)} {r.actor !== 'anon' ? r.actor : ''}
                      </span>
                    </div>
                  </Td>
                  <Td>
                    <Badge variant={actionTone(r.action)} className="font-mono">
                      {r.action}
                    </Badge>
                  </Td>
                  <Td className="max-w-44 truncate font-mono text-xs" title={r.target ?? ''}>
                    {r.target ?? '—'}
                  </Td>
                  <Td className="max-w-56 text-xs text-muted-foreground">
                    <span className="block max-w-56 truncate">{summarizeDetail(r)}</span>
                  </Td>
                  <Td className="font-mono text-xs whitespace-nowrap text-muted-foreground">{ip ?? '—'}</Td>
                </Tr>
              )
            })}
          </TBody>
        </Table>
      )}

      {q.hasNextPage && (
        <Button
          variant="outline"
          size="sm"
          className="self-center"
          disabled={q.isFetchingNextPage}
          onClick={() => void q.fetchNextPage()}
        >
          {t('common:loadMore')}
        </Button>
      )}
      <AuditDetail
        row={selectedRow}
        id={detailId}
        onClose={() => setSelected(null)}
        actor={selectedRow ? actorLabel(selectedRow) : ''}
        kind={selectedRow ? actorKind(selectedRow) : ''}
        onFilter={applyQuickFilter}
      />
    </div>
  )
}

function AuditDetail({ row, id, onClose, actor, kind, onFilter }: {
  row: AuditRow | null
  id: string
  onClose: () => void
  actor: string
  kind: string
  onFilter: (patch: Partial<Draft>) => void
}) {
  const { t } = useTranslation()
  if (!row) return null
  const detail = row.detail
  const hasDiff = detail !== null && ('before' in detail || 'after' in detail)
  const hasDeleted = detail !== null && detail.deleted !== undefined && detail.deleted !== null
  const skip = new Set<string>()
  if (hasDiff) {
    skip.add('before')
    skip.add('after')
    if ('value' in detail && stableEqual(detail.value, detail.after)) skip.add('value')
  }
  if (hasDeleted) skip.add('deleted')
  const entries = detailEntries(detail, true, skip)
  const ip = rowIp(row)
  const ua = rowUa(row)
  const hasContext = row.actor_key_id != null || Boolean(row.trace_id) || Boolean(ua)

  const field = (label: string, content: React.ReactNode) => (
    <div className="min-w-0 space-y-1">
      <dt className="text-xs text-muted-foreground">{label}</dt>
      <dd className="break-words text-sm">{content}</dd>
    </div>
  )

  const filterBtn = (patch: Partial<Draft>) => (
    <Button
      type="button"
      size="xs"
      variant="ghost"
      className="h-6 px-1.5 text-[11px] text-muted-foreground"
      onClick={() => onFilter(patch)}
      title={t('admin:auditFilterBy')}
    >
      <Filter className="h-3 w-3" />
    </Button>
  )

  return (
    <Drawer open onClose={onClose} title={t('admin:auditDetailTitle')} description={t('admin:auditDetailHint')} size="lg">
      <div id={id} className="space-y-5">
        <FieldGroup title={t('admin:auditRecordInfo')}>
          <dl className="grid grid-cols-2 gap-x-5 gap-y-4">
            {field(t('admin:auditRecordId'), <AuditValue value={String(row.id)} />)}
            {field(t('admin:auditTime'), dayjs(row.created_at).format('YYYY-MM-DD HH:mm:ss'))}
            {field(t('admin:auditActor'), (
              <div className="space-y-1">
                <p className="break-all">
                  {actor}
                  {kind && <Badge variant="muted" className="ml-2">{kind}</Badge>}
                </p>
                {row.actor !== 'anon' && (
                  <AuditValue value={row.actor} action={filterBtn({ actor: row.actor })} />
                )}
              </div>
            ))}
            {field(t('admin:auditAction'), (
              <div className="flex items-center gap-1.5">
                <Badge variant={actionTone(row.action)} className="max-w-full break-all whitespace-normal font-mono">
                  {row.action}
                </Badge>
                {filterBtn({ action: row.action })}
              </div>
            ))}
            {field(t('admin:auditTarget'), row.target
              ? <AuditValue value={row.target} action={filterBtn({ target: row.target })} />
              : '—')}
            {field('IP', ip ? <AuditValue value={ip} action={filterBtn({ ip })} /> : '—')}
          </dl>
        </FieldGroup>

        {hasContext && (
          <FieldGroup title={t('admin:auditRequestContext')}>
            <dl className="grid grid-cols-2 gap-x-5 gap-y-4">
              {row.actor_key_id != null && field(t('admin:auditActorKeyId'), <AuditValue value={String(row.actor_key_id)} />)}
              {row.trace_id && field(t('admin:auditTraceId'), <AuditValue value={row.trace_id} action={filterBtn({ trace_id: row.trace_id })} />)}
              {ua && (
                <div className="col-span-2 min-w-0 space-y-1">
                  <dt className="text-xs text-muted-foreground">{t('admin:auditUserAgent')}</dt>
                  <dd className="break-words text-sm"><AuditValue value={ua} /></dd>
                </div>
              )}
            </dl>
          </FieldGroup>
        )}

        {hasDiff && (
          <FieldGroup title={t('admin:auditDiffTitle')}>
            <AuditDiff before={detail.before} after={detail.after} />
          </FieldGroup>
        )}

        {hasDeleted && (
          <FieldGroup title={t('admin:auditDeletedSnapshot')}>
            <div className="rounded-lg border border-destructive/30 bg-destructive/5 p-3">
              <AuditValue value={formatJsonValue(detail.deleted, true)} />
            </div>
          </FieldGroup>
        )}

        <FieldGroup title={hasDiff || hasDeleted ? t('admin:auditRawDetail') : t('admin:auditDetail')}>
          {entries.length ? (
            <dl className="space-y-3">
              {entries.map(([key, value]) => (
                <div key={key} className="min-w-0 space-y-1">
                  <dt className="break-all font-mono text-xs text-muted-foreground">{key}</dt>
                  <dd className="rounded-lg border border-border/60 bg-muted/35 p-3">
                    <AuditValue value={value} />
                  </dd>
                </div>
              ))}
            </dl>
          ) : (
            <p className="text-sm text-muted-foreground">—</p>
          )}
        </FieldGroup>
      </div>
    </Drawer>
  )
}

function AuditDiff({ before, after }: { before: unknown; after: unknown }) {
  const { t } = useTranslation()
  if (isPlainObject(before) && isPlainObject(after)) {
    const keys = Array.from(new Set([...Object.keys(before), ...Object.keys(after)]))
    return (
      <div className="space-y-3">
        {keys.map((k) => {
          const oldVal = before[k]
          const newVal = after[k]
          const changed = !stableEqual(oldVal, newVal)
          return (
            <div key={k} className="min-w-0 space-y-1.5 rounded-lg border border-border/60 p-2.5">
              <div className="flex items-center justify-between gap-2">
                <span className="font-mono text-xs font-medium">{k}</span>
                {!changed && <Badge variant="muted" className="text-[10px]">=</Badge>}
              </div>
              <div className="grid grid-cols-1 gap-2 sm:grid-cols-2">
                <div className={changed ? 'rounded border border-destructive/30 bg-destructive/5 p-2' : 'rounded border border-border/40 bg-muted/20 p-2'}>
                  <div className="mb-1 text-[10px] font-medium text-muted-foreground">{t('admin:auditDiffBefore')}</div>
                  <pre className="whitespace-pre-wrap break-all font-mono text-xs leading-5">{formatJsonValue(oldVal, true)}</pre>
                </div>
                <div className={changed ? 'rounded border border-emerald-500/30 bg-emerald-500/5 p-2' : 'rounded border border-border/40 bg-muted/20 p-2'}>
                  <div className="mb-1 text-[10px] font-medium text-muted-foreground">{t('admin:auditDiffAfter')}</div>
                  <pre className="whitespace-pre-wrap break-all font-mono text-xs leading-5">{formatJsonValue(newVal, true)}</pre>
                </div>
              </div>
            </div>
          )
        })}
      </div>
    )
  }
  return (
    <div className="grid grid-cols-1 gap-2 sm:grid-cols-2">
      <div className="rounded-lg border border-destructive/30 bg-destructive/5 p-3">
        <div className="mb-1 text-xs font-medium text-muted-foreground">{t('admin:auditDiffBefore')}</div>
        <AuditValue value={formatJsonValue(before, true)} />
      </div>
      <div className="rounded-lg border border-emerald-500/30 bg-emerald-500/5 p-3">
        <div className="mb-1 text-xs font-medium text-muted-foreground">{t('admin:auditDiffAfter')}</div>
        <AuditValue value={formatJsonValue(after, true)} />
      </div>
    </div>
  )
}

function AuditValue({ value, action }: { value: string; action?: React.ReactNode }) {
  return (
    <span className="flex min-w-0 items-start gap-1.5">
      <span className="min-w-0 flex-1 whitespace-pre-wrap break-all font-mono text-xs leading-6">{value}</span>
      {action}
      <CopyButton value={value} size="xs" />
    </span>
  )
}
