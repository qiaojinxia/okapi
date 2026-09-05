import { keepPreviousData, useMutation, useQuery } from '@tanstack/react-query'
import { getRouteApi } from '@tanstack/react-router'
import { Fragment, useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'
import {
  ChevronLeft,
  ChevronRight,
  Download,
  RotateCw,
  ScrollText,
  Search,
  Undo2,
} from 'lucide-react'
import type { LogSearch } from '@/routes/admin.logs'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { useConfirm } from '@/components/ui/confirm'
import { CopyText } from '@/components/ui/copy-button'
import { Field } from '@/components/ui/field'
import { Input } from '@/components/ui/input'
import { PageHeader } from '@/components/ui/page'
import { Segmented } from '@/components/ui/segmented'
import { TableSkeleton } from '@/components/ui/skeleton'
import { InlineStat } from '@/components/ui/stat'
import { EmptyState, ErrorState } from '@/components/ui/state'
import { Switch } from '@/components/ui/switch'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { usePermission } from '@/hooks/use-auth'
import { apiFetch } from '@/lib/api'
import { downloadCsv, microToUsd } from '@/lib/csv'
import { describeError } from '@/lib/i18n'
import { formatBp, formatCount, formatMoney, formatMoneyAggregate } from '@/lib/money'
import { qk } from '@/lib/query-keys'
import { cn } from '@/lib/utils'

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
  errors_only: boolean
  hours: number
  /// `datetime-local` 输入值（浏览器本地时区，形如 2026-08-30T00:00）；空串 = 用相对窗口
  from: string
  to: string
}

const DEFAULT_HOURS = 24
const PAGE = 50

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
    errors_only: d.errors_only || undefined,
    // 绝对区间生效时相对窗口无意义，不写进地址
    hours: from !== undefined || d.hours === DEFAULT_HOURS ? undefined : d.hours,
    from,
    to: from === undefined ? undefined : toIso(d.to),
  }
}

const routeApi = getRouteApi('/admin/logs')

interface LogRow {
  ts: string
  request_id: string
  upstream_request_id: string
  log_type: number
  user_id: number
  username: string
  api_key_id: number
  group: string
  model: string
  channel_id: number
  channel_name: string
  channel_key_id: number
  provider: string
  client_type: string
  client_ip: string
  node: string
  usage: {
    prompt_tokens: number
    cached_tokens: number
    completion_tokens: number
    reasoning_tokens: number
  }
  amount_micro: number
  original_amount_micro: number
  discount_micro: number
  /// 上游成本（官方价 × 渠道相对成本系数）；成本采集上线前的历史行为 0。
  upstream_cost_micro: number
  latency_ms: number
  ttft_ms: number
  is_stream: boolean
  retry_count: number
  failover_count: number
  sticky_layer: number
  upstream_status: number
  error_code: string
  is_error: boolean
  ratio_snapshot: string
}

interface StatResp {
  requests: number
  errors: number
  error_rate_bp: number
  tokens: number
  amount_micro: number
  discount_micro: number
  users: number
  cached_tokens: number
  cache_hit_bp: number
  rpm: number
  tpm: number
  rate_source: string
}

function toParams(f: Draft, offset: number): string {
  const p = new URLSearchParams()
  const from = toIso(f.from)
  if (from !== undefined) {
    p.set('from', from)
    const to = toIso(f.to)
    if (to !== undefined) p.set('to', to)
  } else {
    p.set('hours', String(f.hours))
  }
  p.set('limit', String(PAGE))
  if (offset > 0) p.set('offset', String(offset))
  if (f.model.trim()) p.set('model', f.model.trim())
  if (f.user_id.trim()) p.set('user_id', f.user_id.trim())
  if (f.api_key_id.trim()) p.set('api_key_id', f.api_key_id.trim())
  if (f.channel_id.trim()) p.set('channel_id', f.channel_id.trim())
  if (f.error_code.trim()) p.set('error_code', f.error_code.trim())
  if (f.request_id.trim()) p.set('request_id', f.request_id.trim())
  if (f.errors_only) p.set('errors_only', 'true')
  return p.toString()
}

/// 全站日志页（对齐 new-api 的日志页 + 统计条，数据源换成 CH raw）。
///
/// 版面三段：统计条（先给"这批日志整体什么样"）→ 过滤器 → 明细表。
/// 明细行点开展开排障区——请求 ID / 上游请求 ID / 节点 / 重试与切换计数
/// 是工单三件套，放主表列会把表撑到横向滚动，收进展开区各取所需。
export function AdminLogsPage() {
  const { t } = useTranslation()
  const search = routeApi.useSearch()
  const navigate = routeApi.useNavigate()
  const applied = fromSearch(search)
  const [draft, setDraft] = useState<Draft>(applied)
  const [offset, setOffset] = useState(0)

  // 地址变了（看板深链跳过来、浏览器前进后退）→ 表单跟着地址走。
  // 依赖用序列化后的字符串：search 对象每次渲染都是新引用。
  const appliedKey = JSON.stringify(search)
  useEffect(() => {
    setDraft(fromSearch(JSON.parse(appliedKey) as LogSearch))
    setOffset(0)
  }, [appliedKey])

  const commit = (next: Draft) => {
    void navigate({ search: toSearch(next) })
  }

  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        title={t('admin:logsNav')}
        description={t('admin:logsDesc')}
        icon={ScrollText}
        action={
          <RangePicker
            draft={draft}
            onPreset={(h) => commit({ ...draft, hours: h, from: '', to: '' })}
            onRange={(from, to) => setDraft({ ...draft, from, to })}
            onApplyRange={() => commit(draft)}
          />
        }
      />
      <StatBar applied={applied} />
      <FilterBar draft={draft} onChange={setDraft} onApply={() => commit(draft)} />
      <LogTable applied={applied} offset={offset} onOffset={setOffset} />
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
  const onKey = (e: React.KeyboardEvent) => {
    if (e.key === 'Enter') onApplyRange()
  }
  return (
    <div className="flex flex-wrap items-center gap-2">
      <Segmented
        size="sm"
        ariaLabel={t('admin:logsRange')}
        // 绝对区间生效时没有预设被选中：传一个不存在的值让全部熄灭
        value={absolute ? -1 : draft.hours}
        onChange={(h) => onPreset(h)}
        options={options.map((o) => ({ value: o.h, label: o.label }))}
      />
      <span className="mx-1 hidden h-5 w-px bg-border sm:block" aria-hidden />
      <div className="flex items-center gap-1.5">
        <Input
          type="datetime-local"
          className="h-8 w-44 text-xs"
          aria-label={t('admin:logsFrom')}
          value={draft.from}
          onChange={(e) => onRange(e.target.value, draft.to)}
          onKeyDown={onKey}
        />
        <span className="text-xs text-muted-foreground">→</span>
        <Input
          type="datetime-local"
          className="h-8 w-44 text-xs"
          aria-label={t('admin:logsTo')}
          value={draft.to}
          onChange={(e) => onRange(draft.from, e.target.value)}
          onKeyDown={onKey}
        />
        <Button size="sm" variant={absolute ? 'default' : 'outline'} disabled={!absolute} onClick={onApplyRange}>
          {t('admin:logsApplyRange')}
        </Button>
      </div>
    </div>
  )
}

/// 统计条：消耗 / 请求 / 错误率 / 缓存命中 / RPM / TPM 一行速览
/// （new-api 日志页 Stat 语义；RPM/TPM 数据源由后端按"是否带过滤"切换）。
function StatBar({ applied }: { applied: Draft }) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const params = toParams(applied, 0)
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
    <InlineStat label={label} value={value} tone={tone ?? 'default'} />
  )
  const errBp = s?.error_rate_bp ?? 0
  return (
    <Card>
      <CardContent className="flex flex-wrap items-center gap-x-8 gap-y-3 px-5 py-3">
        {cell(t('admin:logsStatSpend'), s ? formatMoneyAggregate(s.amount_micro, locale) : '—')}
        {cell(t('common:requests'), s ? formatCount(s.requests, locale) : '—')}
        {cell(
          t('admin:statErrorRate'),
          s ? formatBp(errBp, locale) : '—',
          errBp >= 500 ? 'bad' : errBp >= 100 ? 'warn' : undefined,
        )}
        {cell(t('admin:kpiTokens'), s ? formatCount(s.tokens, locale) : '—')}
        {cell(t('admin:logsStatCacheHit'), s ? formatBp(s.cache_hit_bp, locale) : '—')}
        {cell(t('admin:logsStatUsers'), s ? formatCount(s.users, locale) : '—')}
        {cell('RPM', s ? formatCount(s.rpm, locale) : '—')}
        {cell('TPM', s ? formatCount(s.tpm, locale) : '—')}
        {s && (
          <Badge variant="muted" title={t('admin:logsRateSourceHint')}>
            {s.rate_source === 'redis' ? t('admin:logsRateLive') : t('admin:logsRateWindow')}
          </Badge>
        )}
      </CardContent>
    </Card>
  )
}

function FilterBar({
  draft,
  onChange,
  onApply,
}: {
  draft: Draft
  onChange: (d: Draft) => void
  onApply: () => void
}) {
  const { t } = useTranslation()
  const text = (
    field: 'model' | 'user_id' | 'api_key_id' | 'channel_id' | 'error_code' | 'request_id',
    label: string,
    placeholder?: string,
  ) => (
    <Field label={label} htmlFor={`lf-${field}`}>
      <Input
        id={`lf-${field}`}
        className="h-8 font-mono text-xs"
        value={draft[field]}
        placeholder={placeholder}
        onChange={(e) => onChange({ ...draft, [field]: e.target.value })}
      />
    </Field>
  )
  const active = [draft.model, draft.user_id, draft.api_key_id, draft.channel_id, draft.error_code, draft.request_id]
    .filter((v) => v.trim() !== '').length + (draft.errors_only ? 1 : 0)
  return (
    <Card>
      <form
        onSubmit={(e) => {
          e.preventDefault()
          onApply()
        }}
      >
        <CardContent className="flex flex-col gap-3 p-4">
          <div className="grid gap-3 sm:grid-cols-3 xl:grid-cols-6">
            {text('model', t('pricing:model'), 'gpt-4o')}
            {text('user_id', t('admin:logsUserId'), '42')}
            {text('api_key_id', t('admin:logsKeyId'), '7')}
            {text('channel_id', t('admin:logsChannelId'), '7')}
            {text('error_code', t('admin:logsErrorCode'), 'upstream_error')}
            {text('request_id', t('admin:logsRequestId'), 'uuid')}
          </div>
          <div className="flex flex-wrap items-center justify-between gap-3">
            <Switch
              checked={draft.errors_only}
              onChange={(v) => onChange({ ...draft, errors_only: v })}
              label={t('admin:logsErrorsOnly')}
            />
            <div className="flex items-center gap-2">
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
                      errors_only: false,
                    })
                  }
                >
                  {t('common:clearFilters')}
                </Button>
              )}
              <Button type="submit" size="sm">
                <Search className="h-3.5 w-3.5" />
                {t('common:search')}
              </Button>
            </div>
          </div>
        </CardContent>
      </form>
    </Card>
  )
}

function LogTable({
  applied,
  offset,
  onOffset,
}: {
  applied: Draft
  offset: number
  onOffset: (v: number) => void
}) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const [expanded, setExpanded] = useState<string | null>(null)
  const params = toParams(applied, offset)
  const q = useQuery({
    queryKey: qk.adminLogs(params),
    queryFn: () => apiFetch<{ data: LogRow[] }>(`/admin/logs?${params}`),
    // 翻页时保留上一页数据，避免表格闪空
    placeholderData: keepPreviousData,
    retry: false,
  })

  if (q.isError) {
    return <ErrorState message={describeError(q.error)} onRetry={() => void q.refetch()} />
  }
  if (q.isPending) {
    return <TableSkeleton rows={10} cols={9} />
  }
  const rows = q.data.data
  return (
    <div className="flex flex-col gap-2">
      {/* 工具栏两组按钮窄屏下允许整组换行；按钮自身永不折行（Button 基类） */}
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="flex gap-2">
          <Button size="sm" variant="outline" loading={q.isFetching} onClick={() => void q.refetch()}>
            {!q.isFetching && <RotateCw className="h-3.5 w-3.5" />}
            {t('common:refresh')}
          </Button>
          {/* 导出当前页：审计/对账拿表格比截图快；全量导出属报表任务不在此 */}
          <Button
            size="sm"
            variant="outline"
            disabled={rows.length === 0}
            onClick={() => exportCsv(rows)}
          >
            <Download className="h-3.5 w-3.5" />
            {t('portal:logsExport')}
          </Button>
        </div>
        {/* CH 明细无 total 计数（count 要多扫一遍），按"整页 = 可能有下一页"翻 */}
        <div className="flex items-center gap-1">
          <span className="mr-1 text-xs text-muted-foreground tabular-nums">
            {t('admin:logsPage', { page: Math.floor(offset / PAGE) + 1 })}
          </span>
          <Button
            size="icon"
            variant="outline"
            className="h-8 w-8"
            aria-label={t('common:prevPage')}
            disabled={offset === 0}
            onClick={() => onOffset(Math.max(0, offset - PAGE))}
          >
            <ChevronLeft className="h-4 w-4" />
          </Button>
          <Button
            size="icon"
            variant="outline"
            className="h-8 w-8"
            aria-label={t('common:nextPage')}
            disabled={rows.length < PAGE}
            onClick={() => onOffset(offset + PAGE)}
          >
            <ChevronRight className="h-4 w-4" />
          </Button>
        </div>
      </div>
      {rows.length === 0 ? (
        <EmptyState hint={t('admin:logsEmptyHint')} />
      ) : (
        <Table dense stickyHeader>
          <THead>
            <Tr>
              <Th className="w-6" />
              <Th>{t('logs:time')}</Th>
              <Th>{t('common:status')}</Th>
              <Th>{t('admin:logsUser')}</Th>
              <Th>{t('pricing:model')}</Th>
              <Th>{t('admin:logsChannel')}</Th>
              <Th numeric>{t('logs:tokens')}</Th>
              <Th numeric>{t('common:amount')}</Th>
              <Th numeric>{t('admin:logsLatencyTtft')}</Th>
              <Th>{t('admin:logsClient')}</Th>
            </Tr>
          </THead>
          <TBody>
            {rows.map((r) => {
              const open = expanded === r.request_id
              return (
              <Fragment key={r.request_id}>
                <Tr
                  className="cursor-pointer"
                  selected={open}
                  aria-expanded={open}
                  onClick={() => setExpanded(open ? null : r.request_id)}
                >
                  <Td className="pr-0 text-muted-foreground">
                    <ChevronRight className={cn('h-3.5 w-3.5 transition-transform', open && 'rotate-90')} />
                  </Td>
                  <Td className="whitespace-nowrap font-mono text-xs text-muted-foreground">{r.ts.slice(5, 19)}</Td>
                  <Td>
                    {r.is_error ? (
                      <Badge dot variant="destructive">{r.error_code || t('logs:failed')}</Badge>
                    ) : (
                      <Badge dot variant="success">{t('logs:ok')}</Badge>
                    )}
                  </Td>
                  <Td className="max-w-28 truncate text-xs">
                    {r.username || `#${r.user_id}`}
                  </Td>
                  {/* 表格已可横向滚动，模型名不该在格内被从中间折断 */}
                  <Td className="whitespace-nowrap font-mono text-xs">{r.model}</Td>
                  <Td className="max-w-32 truncate text-xs">
                    {r.channel_name || (r.channel_id > 0 ? `#${r.channel_id}` : '—')}
                  </Td>
                  <Td numeric className="whitespace-nowrap text-xs">
                    {formatCount(r.usage.prompt_tokens, locale)}
                    {r.usage.cached_tokens > 0 && (
                      <span className="text-muted-foreground">
                        ({t('logs:cachedShort', { n: r.usage.cached_tokens })})
                      </span>
                    )}
                    {' + '}
                    {formatCount(r.usage.completion_tokens, locale)}
                  </Td>
                  <Td numeric className="whitespace-nowrap font-medium">{formatMoney(r.amount_micro, locale)}</Td>
                  <Td numeric className="whitespace-nowrap font-mono text-xs">
                    {r.latency_ms}
                    {r.is_stream && r.ttft_ms > 0 && (
                      <span className="text-muted-foreground"> / {r.ttft_ms}</span>
                    )}
                    ms
                  </Td>
                  <Td className="text-xs text-muted-foreground">{r.client_type || '—'}</Td>
                </Tr>
                {open && (
                  <Tr className="hover:bg-transparent">
                    <Td colSpan={10} className="bg-muted/30 p-0">
                      <RowDetail row={r} />
                    </Td>
                  </Tr>
                )}
              </Fragment>
              )
            })}
          </TBody>
        </Table>
      )}
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

/// 展开区：排障字段全集。分三行——标识（工单锚点）/ 调度（哪条链路怎么走的）/
/// 金额构成。ratio_snapshot 原样给出，倍率争议时直接对着快照讲。
function RowDetail({ row }: { row: LogRow }) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const item = (label: string, value: React.ReactNode) => (
    <div className="flex min-w-0 flex-col gap-0.5">
      <span className="text-[11px] text-muted-foreground">{label}</span>
      <span className="min-w-0 truncate font-mono text-xs">{value}</span>
    </div>
  )
  const section = (title: string, children: React.ReactNode) => (
    <div className="flex min-w-0 flex-col gap-2 rounded-md border border-border bg-card p-3">
      <span className="text-[11px] font-semibold tracking-wide text-muted-foreground uppercase">
        {title}
      </span>
      <div className="grid grid-cols-2 gap-x-4 gap-y-2 sm:grid-cols-3">{children}</div>
    </div>
  )
  return (
    <div className="flex flex-col gap-3 px-4 py-3 text-xs animate-fade-in">
      <div className="grid gap-3 lg:grid-cols-3">
        {section(
          t('admin:logsDetailIdentity'),
          <>
            <div className="col-span-2 flex min-w-0 flex-col gap-0.5 sm:col-span-3">
              <span className="text-[11px] text-muted-foreground">{t('admin:logsRequestId')}</span>
              <CopyText value={row.request_id} />
            </div>
            {row.upstream_request_id && (
              <div className="col-span-2 flex min-w-0 flex-col gap-0.5 sm:col-span-3">
                <span className="text-[11px] text-muted-foreground">{t('admin:logsUpstreamId')}</span>
                <CopyText value={row.upstream_request_id} />
              </div>
            )}
            {item(t('admin:logsNode'), row.node || '—')}
            {row.client_ip && item('IP', row.client_ip)}
            {item(t('logs:group'), row.group)}
            {item('Key', `#${row.api_key_id}`)}
          </>,
        )}
        {section(
          t('admin:logsDetailRouting'),
          <>
            {item(t('admin:provider'), row.provider || '—')}
            {item(t('admin:logsChannelKey'), `#${row.channel_key_id}`)}
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
          <>
            {item(t('logs:original'), formatMoney(row.original_amount_micro, locale))}
            {row.discount_micro > 0 &&
              item(t('logs:discount'), `-${formatMoney(row.discount_micro, locale)}`)}
            {item(t('logs:final'), <strong>{formatMoney(row.amount_micro, locale)}</strong>)}
            {/* 成本与毛利只在有成本数据时出现；负毛利标红——这一笔在亏钱 */}
            {row.upstream_cost_micro > 0 && (
              <>
                {item(t('admin:statUpstreamCost'), formatMoney(row.upstream_cost_micro, locale))}
                {item(
                  t('admin:statMargin'),
                  <span className={row.amount_micro < row.upstream_cost_micro ? 'text-destructive' : 'text-success'}>
                    {formatMoney(row.amount_micro - row.upstream_cost_micro, locale)}
                  </span>,
                )}
              </>
            )}
            {row.ratio_snapshot && (
              <div className="col-span-2 flex min-w-0 flex-col gap-0.5 sm:col-span-3">
                <span className="text-[11px] text-muted-foreground">{t('admin:logsRatioSnapshot')}</span>
                <code className="break-all font-mono text-xs">{row.ratio_snapshot}</code>
              </div>
            )}
          </>,
        )}
      </div>
      <RefundInline row={row} />
    </div>
  )
}
