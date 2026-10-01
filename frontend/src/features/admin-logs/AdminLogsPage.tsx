import { keepPreviousData, useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { getRouteApi } from '@tanstack/react-router'
import { useEffect, useId, useState } from 'react'
import { useTranslation } from 'react-i18next'
import {
  CalendarDays,
  ChevronDown,
  SlidersHorizontal,
  Download,
  RotateCw,
  FileText,
  Search,
  Undo2,
  Fingerprint,
  KeyRound,
  Route,
  Server,
  User,
  Wallet,
} from 'lucide-react'
import type { LucideIcon } from 'lucide-react'
import type { LogSearch } from '@/routes/admin.logs'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { useConfirm } from '@/components/ui/confirm'
import { Field } from '@/components/ui/field'
import { Input } from '@/components/ui/input'
import { Select } from '@/components/ui/select'
import { Drawer } from '@/components/ui/drawer'
import { LogTokenUsage } from '@/features/logs/LogTokenUsage'
import { LogPerformance, LogPerformanceDetails } from '@/features/logs/LogPerformance'
import { LogErrorDetails } from '@/features/logs/LogErrorDetails'
import { DetailSection, IdRow, InfoGrid, InfoItem } from '@/features/logs/detail-ui'
import { LogRequestDetails } from '@/features/logs/LogRequestDetails'
import { LogBillingDetails } from '@/features/logs/LogBillingDetails'
import { billingStatus, logMoney } from '@/features/logs/types'
import { PageHeader } from '@/components/ui/page'
import { Pagination } from '@/components/ui/pagination'
import { Segmented } from '@/components/ui/segmented'
import { RowExpander } from '@/components/ui/row-expander'
import { ModelSearchInput } from '@/features/models/model-input'
import { EntitySearchInput, malformedEntityId, validEntityId } from '@/features/entity-search/EntitySearchInput'
import type { EntityKind, EntityOption } from '@/features/entity-search/EntitySearchInput'
import type { ScopeEcho } from '@/features/analytics/types'
import { TableSkeleton } from '@/components/ui/skeleton'
import { InlineStat } from '@/components/ui/stat'
import { EmptyState, ErrorState } from '@/components/ui/state'
import { Switch } from '@/components/ui/switch'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { usePermission } from '@/hooks/use-auth'
import { DEFAULT_PAGE_SIZE, PAGE_SIZES, type Pager, usePagination } from '@/hooks/use-pagination'
import { apiFetch } from '@/lib/api'
import { downloadCsv, microToUsd } from '@/lib/csv'
import { describeError } from '@/lib/i18n'
import { formatBp, formatCount, formatMoney, formatMoneyAggregate } from '@/lib/money'
import { qk } from '@/lib/query-keys'
import { TokenBreakdown } from '@/features/logs/TokenBreakdown'
import { ExtraMetrics } from '@/features/logs/LogSummary'
import type { LogStats, TokenDetails, LogDiagnostics, Snapshot } from '@/features/logs/types'

/// 检索条件（受控草稿 → 点查询才提交）。
///
/// 不做输入即查：全站明细查询打的是 CH raw 表，模型名敲到一半的每个前缀都
/// 发一发查询既慢又没意义；草稿/已提交两份状态，回车或点按钮才生效。
/// **已提交态 = URL search**（`routes/admin.logs.tsx`），草稿是本地表单值。
interface Draft {
  model: string
  user_id: string
  api_key_id: string
  channel_id: string
  error_code: string
  request_id: string
  upstream_request_id: string
  group: string
  client_type: string
  log_type: string
  errors_only: boolean
  hours: number
  /// `datetime-local` 输入值（浏览器本地时区，形如 2026-08-30T00:00）；空串 = 用相对窗口
  from: string
  to: string
}

const DEFAULT_HOURS = 24
const numericFilters = ['user_id', 'api_key_id', 'channel_id'] as const
function invalidId(value: string): boolean {
  return !validEntityId(value)
}

function validRange(draft: Pick<Draft, 'from' | 'to'>): boolean {
  const from = toIso(draft.from), to = toIso(draft.to)
  return from !== undefined && (draft.to === '' || (to !== undefined && Date.parse(to) >= Date.parse(from)))
}

/// RFC3339（UTC）→ datetime-local 输入值（本地时区，分钟精度）。
function toLocalInput(iso: string | undefined): string {
  if (!iso) return ''
  const d = new Date(iso)
  if (Number.isNaN(d.getTime())) return ''
  const pad = (n: number) => String(n).padStart(2, '0')
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`
}

/// datetime-local → RFC3339（UTC）。地址栏与后端只认 UTC，避免时区随浏览器漂移。
function toIso(local: string): string | undefined {
  if (!local) return undefined
  const d = new Date(local)
  return Number.isNaN(d.getTime()) ? undefined : d.toISOString()
}

/// URL → 表单值（缺省字段回落为空串/缺省窗口）。
function fromSearch(s: LogSearch): Draft {
  return {
    model: s.model ?? '',
    user_id: s.user_id?.toString() ?? '',
    api_key_id: s.api_key_id?.toString() ?? '',
    channel_id: s.channel_id?.toString() ?? '',
    error_code: s.error_code ?? '',
    request_id: s.request_id ?? '',
    upstream_request_id: s.upstream_request_id ?? '',
    group: s.group ?? '',
    client_type: s.client_type ?? '',
    log_type: s.log_type?.toString() ?? '',
    errors_only: s.errors_only === true,
    hours: s.hours ?? DEFAULT_HOURS,
    from: toLocalInput(s.from),
    to: toLocalInput(s.to),
  }
}

/// 表单值 → URL（空值不写进地址栏，保持链接干净；hours 等于缺省也省略）。
function toSearch(d: Draft): LogSearch {
  const id = (v: string) => {
    const n = Number(v.trim())
    return Number.isInteger(n) && n > 0 ? n : undefined
  }
  const from = toIso(d.from)
  return {
    model: d.model.trim() || undefined,
    user_id: id(d.user_id),
    api_key_id: id(d.api_key_id),
    channel_id: id(d.channel_id),
    error_code: d.error_code.trim() || undefined,
    request_id: d.request_id.trim() || undefined,
    upstream_request_id: d.upstream_request_id.trim() || undefined,
    group: d.group.trim() || undefined,
    client_type: d.client_type.trim() || undefined,
    log_type: d.log_type ? Number(d.log_type) : undefined,
    errors_only: d.errors_only || undefined,
    // 绝对区间生效时相对窗口无意义，不写进地址
    hours: from !== undefined || d.hours === DEFAULT_HOURS ? undefined : d.hours,
    from,
    to: from === undefined ? undefined : toIso(d.to),
  }
}

const routeApi = getRouteApi('/admin/logs')

interface LogRow {
  requested_model?: string
  upstream_model?: string
  endpoint?: string
  upstream_endpoint?: string
  usage_details_recorded?: boolean
  ts: string
  request_id: string
  upstream_request_id: string
  log_type: number
  user_id: number
  username: string
  api_key_id: number
  key_name?: string
  key_prefix?: string
  group: string
  model: string
  channel_id: number
  channel_name: string
  channel_key_id: number
  provider: string
  client_type: string
  client_ip: string
  node: string
  usage: TokenDetails
  amount_micro: number
  original_amount_micro: number
  discount_micro: number
  /// 上游成本（官方价 × 渠道相对成本系数）；成本采集上线前的历史行为 0。
  upstream_cost_micro: number
  status?: number | null
  diagnostics?: LogDiagnostics | null
  request_type?: string
  upstream_cost_known?: boolean | null
  latency_ms: number | null
  ttft_ms: number | null
  is_stream: boolean
  retry_count: number
  failover_count: number
  sticky_layer: number
  upstream_status: number
  error_code: string
  is_error: boolean
  ratio_snapshot: string
}

interface StatResp extends Partial<LogStats> {
  requests: number
  errors: number
  error_rate_bp: number
  tokens: number
  amount_micro: number
  discount_micro: number
  users: number
  cached_tokens: number
  cache_hit_bp: number | null
  rpm: number
  tpm: number
  rate_source: string
}

function toParams(f: Draft, offset: number, limit: number): string {
  const p = new URLSearchParams()
  const from = toIso(f.from)
  if (from !== undefined) {
    p.set('from', from)
    const to = toIso(f.to)
    if (to !== undefined) p.set('to', to)
  } else {
    p.set('hours', String(f.hours))
  }
  p.set('limit', String(limit))
  if (offset > 0) p.set('offset', String(offset))
  if (f.model.trim()) p.set('model', f.model.trim())
  if (f.user_id.trim()) p.set('user_id', f.user_id.trim())
  if (f.api_key_id.trim()) p.set('api_key_id', f.api_key_id.trim())
  if (f.channel_id.trim()) p.set('channel_id', f.channel_id.trim())
  if (f.error_code.trim()) p.set('error_code', f.error_code.trim())
  if (f.request_id.trim()) p.set('request_id', f.request_id.trim())
  for (const key of ['group', 'client_type', 'upstream_request_id', 'log_type'] as const) {
    if (f[key].trim()) p.set(key, f[key].trim())
  }
  if (f.errors_only) p.set('errors_only', 'true')
  return p.toString()
}

/// 全站日志页（对齐 new-api 的日志页 + 统计条，数据源换成 CH raw）。
///
/// 版面三段：紧凑检索工具栏 → 窗口统计条 → 铺满剩余高度的明细表。
/// 明细行点开详情抽屉——请求 ID / 上游请求 ID / 节点 / 重试与切换计数
/// 是工单三件套，放主表列会把表撑到横向滚动，收进展开区各取所需。
export function AdminLogsPage() {
  const { t } = useTranslation()
  const client = useQueryClient()
  const search = routeApi.useSearch()
  const navigate = routeApi.useNavigate()
  const applied = fromSearch(search)
  const [draft, setDraft] = useState<Draft>(applied)

  // 地址变了（看板深链跳过来、浏览器前进后退）→ 表单跟着地址走。
  // 依赖用序列化后的字符串：search 对象每次渲染都是新引用；只看过滤条件，翻页不算。
  const appliedKey = JSON.stringify(toSearch(applied))
  useEffect(() => {
    setDraft(fromSearch(JSON.parse(appliedKey) as LogSearch))
  }, [appliedKey])
  // 页宽档位到 200 为止（后端钳制上限）；CH 明细不 count，翻页靠"本页满 = 可能还有"。
  // 页码也在地址里；`commit` 整体替换 search 时不带 page，过滤一变自然回第一页
  const pager = usePagination({ pageSizes: [...PAGE_SIZES, 200] })
  const logs = useAdminLogs(applied, pager)
  const rows = logs.isError || logs.isPlaceholderData ? [] : logs.data?.data ?? []
  const scope = logs.isError || logs.isPlaceholderData ? undefined : logs.data?.scope
  const known: Partial<Record<EntityKind, EntityOption[]>> = {
    user: rows.filter((row) => row.username).map((row) => ({ id: row.user_id, name: row.username })),
    channel: rows.filter((row) => row.channel_name && row.channel_id > 0).map((row) => ({ id: row.channel_id, name: row.channel_name, description: row.provider })),
    api_key: rows.filter((row) => row.key_name || row.key_prefix).map((row) => ({ id: row.api_key_id, name: row.key_name || t('flow:unnamed_api_key'), description: [row.username, row.key_prefix ? `${row.key_prefix}…` : undefined].filter(Boolean).join(' · ') })),
  }
  // 回填按已提交 ID 匹配；旧页占位数据和其他对象的名字不能套到新条件上。
  if (scope?.user && scope.user.id === search.user_id && scope.user.username != null) known.user?.push({ id: scope.user.id, name: scope.user.username || t('flow:unnamed_user') })
  if (scope?.channel && scope.channel.id === search.channel_id && scope.channel.name != null) known.channel?.push({ id: scope.channel.id, name: scope.channel.name || t('flow:unnamed_channel'), description: scope.channel.provider ?? undefined })
  if (scope?.api_key && scope.api_key.id === search.api_key_id && scope.api_key.name != null) known.api_key?.push({
    id: scope.api_key.id, name: scope.api_key.name || t('flow:unnamed_api_key'),
    description: [scope.api_key.username, scope.api_key.key_prefix ? `${scope.api_key.key_prefix}…` : undefined].filter(Boolean).join(' · '),
  })

  const commit = (next: Draft) => {
    if (numericFilters.some((field) => invalidId(next[field])) || ((next.from || next.to) && !validRange(next))) return
    void navigate({ search: toSearch(next) })
  }

  return (
    <div className="list-page [--page-gap:8px]">
      <PageHeader
        title={t('admin:logsNav')}
        description={t('admin:logsDesc')}
        icon={FileText}
        compact
        className="[&_p]:text-xs [&_p]:leading-5"
        action={<>
          <Button size="sm" variant="outline" disabled={rows.length === 0 || logs.isFetching} title={t('logs:exportPageHint', { n: rows.length })} onClick={() => exportCsv(rows)}>
            <Download className="h-3.5 w-3.5" />{t('logs:exportPage')}
          </Button>
          <Button size="sm" variant="outline" loading={logs.isFetching} onClick={() => {
            void logs.refetch()
            void client.invalidateQueries({ queryKey: qk.adminLogStat(toParams(applied, 0, DEFAULT_PAGE_SIZE)) })
          }}>
            {!logs.isFetching && <RotateCw className="h-3.5 w-3.5" />}{t('common:refresh')}
          </Button>
        </>}
      />
      <Card aria-label={t('logs:filters')} data-slot="admin-log-filters" className="shrink-0 rounded-xl px-2 py-1">
        <FilterBar draft={draft} onChange={setDraft} onApply={() => commit(draft)} known={known} />
        <div className="flex min-w-0 flex-wrap items-start gap-2 border-t border-border/60 pt-2">
          <RangePicker
            draft={draft}
            onPreset={(h) => commit({ ...draft, hours: h, from: '', to: '' })}
            onRange={(from, to) => setDraft({ ...draft, from, to })}
            onApplyRange={() => commit(draft)}
          />
        </div>
      </Card>
      <StatBar applied={applied} />
      <LogTable applied={applied} pager={pager} q={logs} />
    </div>
  )
}

/// 时间窗：四档相对预设（1h 排障、24h 日常、7d 周报、30d 月度）+ 绝对起止
/// （对账"某一天的账"）。两者互斥：填了起止预设全部熄灭；点预设清空起止。
/// 起止不做输入即查——两个时间要一起填完才有意义，回车或点"应用"才提交。
function RangePicker({
  draft,
  onPreset,
  onRange,
  onApplyRange,
}: {
  draft: Draft
  onPreset: (h: number) => void
  onRange: (from: string, to: string) => void
  onApplyRange: () => void
}) {
  const { t } = useTranslation()
  const options = [
    { h: 1, label: t('admin:logsHours1') },
    { h: 24, label: t('admin:logsHours24') },
    { h: 168, label: t('admin:logsHours168') },
    { h: 720, label: t('admin:logsHours720') },
  ]
  const absolute = draft.from !== ''
  const [customOpen, setCustomOpen] = useState(Boolean(draft.from || draft.to))
  useEffect(() => { if (draft.from || draft.to) setCustomOpen(true) }, [draft.from, draft.to])
  const valid = validRange(draft)
  const onKey = (e: React.KeyboardEvent) => {
    if (e.key === 'Enter' && !e.nativeEvent.isComposing) { e.preventDefault(); if (valid) onApplyRange() }
  }
  return (
    <section aria-label={t('admin:logsRange')} className="flex min-w-0 flex-1 basis-96 flex-wrap items-start gap-x-3 gap-y-2">
      <Segmented
        size="sm"
        ariaLabel={t('admin:logsRange')}
        // 绝对区间生效时没有预设被选中：传一个不存在的值让全部熄灭
        value={absolute ? -1 : draft.hours}
        onChange={(h) => { setCustomOpen(false); onPreset(h) }}
        options={options.map((o) => ({ value: o.h, label: o.label }))}
      />
      <details open={customOpen} onToggle={(event) => setCustomOpen(event.currentTarget.open)} className="min-w-0 max-w-full open:w-full">
        <summary className="flex min-h-9 cursor-pointer items-center gap-2 rounded-md px-2 text-sm outline-none hover:bg-muted focus-visible:ring-2 focus-visible:ring-primary/40">
          <CalendarDays aria-hidden className="h-4 w-4 shrink-0 text-muted-foreground" />
          <span className="break-words">{absolute ? `${draft.from.replace('T', ' ')} — ${draft.to.replace('T', ' ') || t('admin:logsUntilNow')}` : t('admin:logsCustomRange')}</span>
        </summary>
        <div className="mt-2 grid min-w-0 gap-3 border-t border-border pt-3 sm:grid-cols-[minmax(0,20rem)_minmax(0,20rem)_max-content] sm:items-end">
          <Field label={t('admin:logsFrom')} htmlFor="logs-from">
            <Input
              id="logs-from"
              type="datetime-local"
              className="h-11 min-w-0 w-full md:h-9"
              aria-label={t('admin:logsFrom')}
              value={draft.from}
              onChange={(e) => onRange(e.target.value, draft.to)}
              onKeyDown={onKey}
            />
          </Field>
          <Field label={t('admin:logsTo')} htmlFor="logs-to">
            <Input
              id="logs-to"
              type="datetime-local"
              className="h-11 min-w-0 w-full md:h-9"
              aria-label={t('admin:logsTo')}
              value={draft.to}
              onChange={(e) => onRange(draft.from, e.target.value)}
              onKeyDown={onKey}
            />
          </Field>
          <Button className="min-h-11 md:min-h-9" variant={absolute ? 'default' : 'outline'} disabled={!valid} onClick={onApplyRange}>
            {t('admin:logsApplyRange')}
          </Button>
        </div>
        <p className="mt-2 text-xs text-muted-foreground">{t('admin:logsLocalTime')}</p>
        {(draft.from || draft.to) && !valid && <p role="alert" className="mt-1 text-xs text-destructive">{t('admin:logsInvalidRange')}</p>}
      </details>
    </section>
  )
}

/// 统计条：消耗 / 请求 / 错误率 / 缓存命中 / RPM / TPM 一行速览
/// （new-api 日志页 Stat 语义；RPM/TPM 数据源由后端按"是否带过滤"切换）。
function StatBar({ applied }: { applied: Draft }) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const [expanded, setExpanded] = useState(false)
  // 统计不分页：使用固定参数，不随明细表换页宽而重新请求。
  const params = toParams(applied, 0, DEFAULT_PAGE_SIZE)
  const q = useQuery({
    queryKey: qk.adminLogStat(params),
    queryFn: () => apiFetch<StatResp>(`/admin/logs/stat?${params}`),
    refetchInterval: 15_000,
    retry: false,
  })

  if (q.isError) {
    // CH 未启用时统计条静默收起，不挡明细排障（明细也会 501，但让表格报即可）
    return null
  }
  const s = q.data
  const cell = (label: string, value: string, tone?: 'warn' | 'bad') => (
    <InlineStat label={label} value={value} tone={tone ?? 'default'} className="px-3 py-2" />
  )
  const errBp = s?.error_rate_bp ?? 0
  return (
    <Card role="region" aria-label={t('logs:summary')} data-slot="admin-log-summary" className="shrink-0 overflow-hidden rounded-xl">
      <div className="flex min-w-0 flex-wrap items-center">
        <div className="grid min-w-0 flex-1 basis-[40rem] grid-cols-4 md:grid-cols-8 [&>div:not(:first-child)]:border-l [&>div]:border-border/60">
          {cell(t('admin:logsStatSpend'), s ? formatMoneyAggregate(s.amount_micro, locale) : '—')}
          {cell(t('common:requests'), s ? formatCount(s.requests, locale) : '—')}
          {cell(
            t('admin:statErrorRate'),
            s ? formatBp(errBp, locale) : '—',
            errBp >= 500 ? 'bad' : errBp >= 100 ? 'warn' : undefined,
          )}
          {cell(t('admin:kpiTokens'), s ? formatCount(s.tokens, locale) : '—')}
          {cell(t('admin:logsStatCacheHit'), s?.cache_hit_bp != null ? formatBp(s.cache_hit_bp, locale) : '—')}
          {cell(t('admin:logsStatUsers'), s ? formatCount(s.users, locale) : '—')}
          {cell('RPM', s ? formatCount(s.rpm, locale) : '—')}
          {cell('TPM', s ? formatCount(s.tpm, locale) : '—')}
        </div>
        <div className="flex w-full shrink-0 items-center justify-end gap-3 border-t border-border/60 px-3 py-1.5 md:w-auto md:flex-col md:items-end md:gap-1 md:border-t-0 md:py-2">
          {s && (
            <Badge variant="muted" title={t('admin:logsRateSourceHint')}>
              {s.rate_source === 'redis' ? t('admin:logsRateLive') : t('admin:logsRateWindow')}
            </Badge>
          )}
          <button type="button" aria-expanded={expanded} onClick={() => setExpanded(!expanded)} className="flex items-center gap-1 rounded text-xs text-primary outline-none focus-visible:ring-2 focus-visible:ring-primary/40">
            {t('logs:moreMetrics')}<ChevronDown aria-hidden className={`h-3.5 w-3.5 transition-transform ${expanded ? 'rotate-180' : ''}`} />
          </button>
        </div>
      </div>
      <ExtraMetrics data={s} expanded={expanded} />
    </Card>
  )
}

function FilterBar({
  draft,
  onChange,
  onApply,
  known,
}: {
  draft: Draft
  onChange: (d: Draft) => void
  onApply: () => void
  known: Partial<Record<EntityKind, EntityOption[]>>
}) {
  const { t } = useTranslation()
  const advancedValues = [draft.user_id, draft.api_key_id, draft.channel_id, draft.error_code, draft.request_id, draft.upstream_request_id, draft.group, draft.client_type, draft.log_type]
  const advancedKey = JSON.stringify(advancedValues)
  const advancedCount = advancedValues.filter((value) => value.trim()).length
  const [advancedOpen, setAdvancedOpen] = useState(advancedCount > 0)
  const advancedId = useId()
  const [blurred, setBlurred] = useState<Partial<Record<(typeof numericFilters)[number], boolean>>>({})
  useEffect(() => { if ((JSON.parse(advancedKey) as string[]).some((value) => value.trim())) setAdvancedOpen(true) }, [advancedKey])
  const invalid = numericFilters.some((field) => invalidId(draft[field])) || Boolean((draft.from || draft.to) && !validRange(draft))
  const text = (
    field: 'error_code' | 'request_id' | 'upstream_request_id' | 'group' | 'client_type',
    label: string,
    placeholder?: string,
  ) => (
    <Field label={label} htmlFor={`lf-${field}`}>
      <Input
        id={`lf-${field}`}
        className="h-11 font-mono text-sm md:h-9 md:text-xs"
        value={draft[field]}
        placeholder={placeholder}
        onChange={(e) => onChange({ ...draft, [field]: e.target.value })}
      />
    </Field>
  )
  const entity = (field: (typeof numericFilters)[number], kind: EntityKind, label: string) => {
    const error = invalidId(draft[field]) && (blurred[field] || malformedEntityId(draft[field]))
    return <Field label={label} htmlFor={`lf-${field}`}>
      <EntitySearchInput id={`lf-${field}`} kind={kind} knownOptions={known[kind]} value={draft[field]}
        inputClassName="h-11 md:h-9"
        aria-invalid={error || undefined} aria-describedby={error ? `lf-${field}-error` : undefined}
        onChange={(value) => { setBlurred((prev) => ({ ...prev, [field]: false })); onChange({ ...draft, [field]: value }) }}
        onBlur={() => setBlurred((prev) => ({ ...prev, [field]: true }))}
        onSubmit={() => { setBlurred((prev) => ({ ...prev, [field]: true })); if (!invalid) onApply() }} />
      {error && <span id={`lf-${field}-error`} className="text-xs text-destructive">{t(malformedEntityId(draft[field]) ? 'analytics:invalidFilterId' : 'analytics:entitySelectionRequired')}</span>}
    </Field>
  }
  const active = [draft.model, draft.user_id, draft.api_key_id, draft.channel_id, draft.error_code, draft.request_id, draft.upstream_request_id, draft.group, draft.client_type, draft.log_type]
    .filter((v) => v.trim() !== '').length + (draft.errors_only ? 1 : 0)
  return (
      <form
        className="min-w-0"
        onSubmit={(e) => {
          e.preventDefault()
          if (!invalid) onApply()
        }}
        onKeyDown={(event) => { if (event.key === 'Enter' && event.nativeEvent.isComposing) event.preventDefault() }}
      >
        <CardContent className="flex flex-col gap-2 p-0 pb-2">
          <div className="flex min-w-0 flex-wrap items-center gap-2 md:gap-3">
            <ModelSearchInput id="lf-model" className="w-full sm:w-64" aria-label={t('pricing:model')}
              inputClassName="h-11 md:h-9" placeholder={t('portal:logsModelHint')}
              value={draft.model} onChange={(model) => onChange({ ...draft, model })} onSubmit={() => { if (!invalid) onApply() }} />
            <Switch
              checked={draft.errors_only}
              onChange={(v) => onChange({ ...draft, errors_only: v })}
              label={t('admin:logsErrorsOnly')}
            />
            <Button type="button" size="sm" variant="ghost" aria-expanded={advancedOpen} aria-controls={advancedId} onClick={() => setAdvancedOpen(!advancedOpen)} className="min-h-11 text-muted-foreground md:min-h-9">
              <SlidersHorizontal aria-hidden className="h-3.5 w-3.5" />{t('admin:logsMoreFilters')}
              {advancedCount > 0 && <Badge variant="muted">{advancedCount}</Badge>}
              <ChevronDown aria-hidden className={`h-3.5 w-3.5 transition-transform ${advancedOpen ? 'rotate-180' : ''}`} />
            </Button>
            <div className="ml-auto flex max-w-full flex-wrap items-center gap-2">
              {active > 0 && (
                <Button
                  type="button"
                  size="sm"
                  variant="ghost"
                  onClick={() =>
                    onChange({
                      ...draft,
                      model: '',
                      user_id: '',
                      api_key_id: '',
                      channel_id: '',
                      error_code: '',
                      request_id: '',
                      upstream_request_id: '', group: '', client_type: '', log_type: '',
                      errors_only: false,
                    })
                  }
                >
                  {t('common:clearFilters')}
                </Button>
              )}
              <Button type="submit" size="sm" className="min-h-11 md:min-h-9" disabled={invalid}>
                <Search className="h-3.5 w-3.5" />
                {t('common:search')}
              </Button>
            </div>
          </div>
          <div id={advancedId} hidden={!advancedOpen} className="min-w-0 border-t border-border/60 pt-2">
            <div className="grid items-start gap-2 md:grid-cols-[repeat(3,minmax(0,20rem))]">
              {entity('user_id', 'user', t('analytics:dimUser'))}
              {entity('api_key_id', 'api_key', t('analytics:dimApiKey'))}
              {entity('channel_id', 'channel', t('analytics:dimChannel'))}
            </div>
            <div className="mt-2 grid gap-2 sm:grid-cols-[minmax(0,20rem)_minmax(0,32rem)]">
              {text('error_code', t('admin:logsErrorCode'), 'upstream_error')}
              {text('request_id', t('admin:logsRequestId'), 'uuid')}
              {text('upstream_request_id', t('admin:logsUpstreamId'))}
              {text('group', t('logs:group'))}
              {text('client_type', t('admin:logsClientType'), 'codex / claude_code / curl')}
              <Field label={t('logs:filterType')} htmlFor="lf-log_type">
                <Select id="lf-log_type" className="w-full" value={draft.log_type} onChange={(log_type) => onChange({ ...draft, log_type })}
                  placeholder={t('common:all')} options={[1, 2, 3, 4, 5, 6, 7].map(type => ({ value: String(type), label: t(`logs:logType_${type}`) }))} />
              </Field>
            </div>
          </div>
        </CardContent>
      </form>
  )
}

function useAdminLogs(applied: Draft, pager: Pager) {
  const params = toParams(applied, pager.offset, pager.limit)
  return useQuery({
    queryKey: qk.adminLogs(params),
    queryFn: () => apiFetch<{ data: LogRow[]; scope?: ScopeEcho }>(`/admin/logs?${params}`),
    // 翻页时保留上一页数据，避免表格闪空
    placeholderData: keepPreviousData,
    retry: false,
  })
}

function LogTable({ applied, pager, q }: { applied: Draft; pager: Pager; q: ReturnType<typeof useAdminLogs> }) {
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
        <Table dense stickyHeader aria-label={t('admin:logsNav')} scrollResetKey={params} className="min-w-[84rem] table-fixed 2xl:min-w-[88rem]" wrapperClassName="[container-type:inline-size]">
          <colgroup>
            <col className="w-6" /><col className="w-32" /><col className="w-36" /><col className="w-36" />
            <col /><col className="w-32 2xl:w-44" /><col className="w-76" /><col className="w-28" /><col className="w-28" /><col className="w-16" />
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
                  <Td className="truncate text-xs" title={r.channel_name || undefined}>
                    {r.channel_name || (r.channel_id > 0 ? `ID ${r.channel_id}` : t('admin:dashboardUnassignedChannel'))}
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

function exportCsv(rows: LogRow[]) {
  downloadCsv(
    'okapi-admin-logs',
    [
      'time',
      'status',
      'error_code',
      'user_id',
      'username',
      'api_key_id',
      'group',
      'model',
      'channel_id',
      'channel_name',
      'provider',
      'client_type',
      'prompt_tokens',
      'cached_tokens',
      'completion_tokens',
      'reasoning_tokens',
      'amount_usd',
      'original_usd',
      'discount_usd',
      'latency_ms',
      'ttft_ms',
      'stream',
      'retry_count',
      'failover_count',
      'upstream_status',
      'request_id',
      'upstream_request_id',
      'node',
      'key_name',
      'key_prefix',
      'requested_model', 'upstream_model', 'endpoint', 'upstream_endpoint',
      'cache_write_tokens', 'cache_read_reported', 'cache_write_reported', 'cache_write_5m_tokens', 'cache_write_1h_tokens',
      'audio_prompt_tokens', 'image_prompt_tokens', 'audio_completion_tokens', 'image_completion_tokens',
      'cache_read_audio_tokens', 'cache_read_image_tokens', 'cache_write_audio_tokens', 'cache_write_image_tokens',
      'prompt_source', 'completion_source', 'upstream_prompt_tokens', 'upstream_completion_tokens',
    ],
    rows.map((r) => [
      r.ts,
      r.is_error ? 'error' : 'ok',
      r.error_code,
      r.user_id,
      r.username,
      r.api_key_id,
      r.group,
      r.model,
      r.channel_id,
      r.channel_name,
      r.provider,
      r.client_type,
      r.usage.prompt_tokens,
      r.usage.cached_tokens,
      r.usage.completion_tokens,
      r.usage.reasoning_tokens,
      microToUsd(r.amount_micro),
      microToUsd(r.original_amount_micro),
      microToUsd(r.discount_micro),
      r.latency_ms,
      r.ttft_ms,
      r.is_stream ? 1 : 0,
      r.retry_count,
      r.failover_count,
      r.upstream_status,
      r.request_id,
      r.upstream_request_id,
      r.node,
      r.key_name ?? '',
      r.key_prefix ?? '',
      r.requested_model, r.upstream_model, r.endpoint, r.upstream_endpoint,
      r.usage.cache_write_tokens, r.usage.cache_read_reported, r.usage.cache_write_reported, r.usage.cache_write_5m_tokens, r.usage.cache_write_1h_tokens,
      r.usage.audio_prompt_tokens, r.usage.image_prompt_tokens, r.usage.audio_completion_tokens, r.usage.image_completion_tokens,
      r.usage.cache_read_modalities?.audio_tokens, r.usage.cache_read_modalities?.image_tokens, r.usage.cache_write_modalities?.audio_tokens, r.usage.cache_write_modalities?.image_tokens,
      r.usage.prompt_source, r.usage.completion_source, r.usage.upstream_usage?.prompt_tokens, r.usage.upstream_usage?.completion_tokens,
    ]),
  )
}

interface RefundResp {
  outcome: 'refunded' | 'already_refunded'
  refunded_micro?: number
  balance_after_micro?: number
}

/// 行内退款（§5.3 按日志退款，#1790-10）——运维页的退款卡要先去日志页复制
/// request_id 再回去粘贴；而管理员正是在看到这条日志时决定要退的，动作就该在这里。
/// 复用同一后端端点（幂等：重复提交回 already_refunded），
/// 只对"成功且扣了钱"的消费行显示；权限点 billing.refund 不足则整块不出现。
function RefundInline({ row }: { row: LogRow }) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const can = usePermission()
  const { confirm, dialog } = useConfirm()
  const [open, setOpen] = useState(false)
  const [reason, setReason] = useState('')
  const [done, setDone] = useState<string | null>(null)

  const refund = useMutation({
    mutationFn: () =>
      apiFetch<RefundResp>('/admin/billing/refund', {
        method: 'POST',
        body: { request_id: row.request_id, reason: reason.trim() },
      }),
    onSuccess: (r) => {
      setDone(
        r.outcome === 'refunded'
          ? t('admin:refundDone', {
              amount: formatMoney(r.refunded_micro ?? 0, locale),
              balance: formatMoney(r.balance_after_micro ?? 0, locale),
            })
          : t('admin:refundAlready'),
      )
      setOpen(false)
    },
    onError: (err) => setDone(describeError(err)),
  })

  if (!can('billing.refund') || row.is_error || row.log_type !== 2 || row.amount_micro <= 0) {
    return null
  }
  if (done !== null) {
    return <span className="text-xs text-muted-foreground">{done}</span>
  }
  return (
    <div className="flex flex-wrap items-center gap-2">
      {dialog}
      {open ? (
        <>
          <Input
            className="h-7 w-64 text-xs"
            placeholder={t('admin:refundReason')}
            value={reason}
            onChange={(e) => setReason(e.target.value)}
          />
          <Button
            size="sm"
            variant="destructive"
            disabled={refund.isPending}
            onClick={() =>
              confirm({
                title: t('admin:refundTitle'),
                description: t('admin:refundConfirm', {
                  user: row.username || `#${row.user_id}`,
                  amount: formatMoney(row.amount_micro, locale),
                }),
                confirmLabel: t('admin:refund'),
                onConfirm: () => refund.mutate(),
              })
            }
          >
            {t('admin:refund')}
          </Button>
          <Button size="sm" variant="ghost" onClick={() => setOpen(false)}>
            {t('common:cancel')}
          </Button>
        </>
      ) : (
        <Button size="sm" variant="outline" onClick={() => setOpen(true)}>
          <Undo2 className="mr-1 h-3.5 w-3.5" />
          {t('admin:refund')}
        </Button>
      )}
    </div>
  )
}

/// 详情抽屉：排障字段全集。分区——标识（工单锚点）/ 调度（哪条链路怎么走的）/
/// 金额构成。ratio_snapshot 原样给出，倍率争议时直接对着快照讲。
function parseSnapshot(raw: string): Snapshot | null {
  try { const value = JSON.parse(raw); return value && typeof value === 'object' && typeof value.mode === 'string' ? value as Snapshot : null }
  catch { return null }
}

function RowDetail({ row }: { row: LogRow }) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const item = (label: string, value: React.ReactNode) => <InfoItem label={label}>{value}</InfoItem>
  const section = (title: string, icon: LucideIcon, children: React.ReactNode) => (
    <DetailSection icon={icon} title={title}><InfoGrid cols={3}>{children}</InfoGrid></DetailSection>
  )
  return (
    <div className="flex min-w-0 flex-col gap-4 text-xs">
      <LogErrorDetails failed={row.is_error} code={row.error_code} upstreamStatus={row.upstream_status} diagnostics={row.diagnostics} />
      {/* 实扣：详情里最先要看的数。成本与毛利只在有成本数据时出现；负毛利标红——这一笔在亏钱 */}
      <dl className="grid gap-4 overflow-hidden rounded-xl border border-primary/20 bg-gradient-to-br from-primary/12 via-card to-card p-5 shadow-xs sm:grid-cols-[1fr_auto] sm:items-end">
        <div className="min-w-0">
          <dt className="text-xs font-medium text-muted-foreground">{t('logs:final')}</dt>
          <dd className="mt-1.5 text-3xl leading-9 font-semibold tracking-tight tabular-nums">{formatMoney(row.amount_micro, locale)}</dd>
        </div>
        {(row.upstream_cost_known === true || row.upstream_cost_micro > 0) && <div className="min-w-36 rounded-lg bg-card/75 px-3 py-2 ring-1 ring-border/60">
          <dt className="text-xs text-muted-foreground">{t('admin:statMargin')}</dt>
          <dd className={`mt-0.5 text-base leading-6 font-semibold tabular-nums ${row.amount_micro < row.upstream_cost_micro ? 'text-destructive' : 'text-success'}`}>{formatMoney(row.amount_micro - row.upstream_cost_micro, locale)}</dd>
        </div>}
      </dl>
      <dl aria-label={t('admin:logsObjects')} className="grid gap-3 sm:grid-cols-3">
        {[
          { label: t('analytics:dimUser'), icon: User, name: row.username, id: row.user_id },
          { label: t('analytics:dimApiKey'), icon: KeyRound, name: row.key_name || (row.key_prefix ? t('flow:unnamed_api_key') : undefined), id: row.api_key_id, prefix: row.key_prefix },
          { label: t('analytics:dimChannel'), icon: Server, name: row.channel_name || (row.channel_id <= 0 ? t('admin:dashboardUnassignedChannel') : undefined), id: row.channel_id },
        ].map((object) => <div key={object.label} className="min-w-0 space-y-1 rounded-xl border border-border bg-card p-3.5 shadow-xs">
          <dt className="flex items-center gap-1.5 text-xs text-muted-foreground"><span aria-hidden className="flex h-5 w-5 shrink-0 items-center justify-center rounded-md bg-primary/10 text-primary"><object.icon className="h-3 w-3" /></span>{object.label}</dt>
          <dd className="break-words text-sm leading-6 font-semibold [overflow-wrap:anywhere]">{object.name || t('admin:logsNameUnavailable')}</dd>
          <dd className="break-all text-xs text-muted-foreground tabular-nums">{object.id > 0 ? `ID ${object.id}` : '—'}{object.prefix ? ` · ${object.prefix}…` : ''}</dd>
        </div>)}
      </dl>
      {section(
        t('admin:logsDetailIdentity'),
        Fingerprint,
        <>
          <div className="col-span-full grid gap-2">
            <IdRow label={t('admin:logsRequestId')} value={row.request_id} />
            {row.upstream_request_id && <IdRow label={t('admin:logsUpstreamId')} value={row.upstream_request_id} />}
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
          {item(t('admin:logsChannelKey'), row.channel_key_id > 0 ? `ID ${row.channel_key_id}` : '—')}
          {item(t('admin:logsUpstreamStatus'), String(row.upstream_status || '—'))}
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
    </div>
  )
}
