import { useInfiniteQuery } from '@tanstack/react-query'
import dayjs from 'dayjs'
import { ChevronRight, Download, FileText, RotateCw, Search } from 'lucide-react'
import { Fragment, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { CopyText } from '@/components/ui/copy-button'
import { Label } from '@/components/ui/input'
import { PageHeader, Toolbar } from '@/components/ui/page'
import { SearchInput } from '@/components/ui/search-input'
import { Segmented } from '@/components/ui/segmented'
import { TableSkeleton } from '@/components/ui/skeleton'
import { EmptyState, ErrorState } from '@/components/ui/state'
import { Switch } from '@/components/ui/switch'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { apiFetch } from '@/lib/api'
import { downloadCsv, microToUsd } from '@/lib/csv'
import { describeError } from '@/lib/i18n'
import { formatMoney } from '@/lib/money'
import { qk } from '@/lib/query-keys'
import { cn } from '@/lib/utils'

interface AppliedRule {
  code: string
  kind: string
  multiplier: string
}

interface Snapshot {
  mode: string
  model_ratio: string | null
  completion_ratio: string | null
  cache_ratio: string | null
  group: string
  group_ratio: string
  user_multiplier: string
  rules: AppliedRule[]
}

interface LogRow {
  id: number
  request_id: string
  model: string
  log_type: number
  status: number
  api_key_id: number | null
  key_name: string
  usage: {
    prompt_tokens: number
    cached_tokens: number
    completion_tokens: number
    reasoning_tokens: number
  }
  amount_micro: number
  original_amount_micro: number
  discount_micro: number
  pricing_snapshot: Snapshot | null
  error_code: string | null
  latency_ms: number | null
  ttft_ms: number | null
  is_stream: boolean
  created_at: string
}

interface LogsResp {
  scope: string
  data: LogRow[]
  next_before: number | null
}

type Scope = 'key' | 'user'

interface Filter {
  scope: Scope
  model: string
  errorsOnly: boolean
}

const PAGE = 50

function params(f: Filter, before: number | null): string {
  const p = new URLSearchParams()
  p.set('limit', String(PAGE))
  p.set('scope', f.scope)
  if (f.model.trim()) p.set('model', f.model.trim())
  if (f.errorsOnly) p.set('errors_only', 'true')
  if (before !== null) p.set('before', String(before))
  return p.toString()
}

/// 用户用量日志（对齐 new-api 用户日志页：按令牌/模型过滤、翻页、首字耗时）。
///
/// 与管理端日志页的分工：那边看渠道/重试/节点（排障），这边看**账**——
/// 每行可展开账单解释器；数据源是 PG 账本（billing_records）而非 CH 明细，
/// 因为用户对账要的是"扣了多少钱、为什么"，账本是唯一权威。
/// 缺省 `scope=key`：合作商员工只见自己那把 key 的记录，与总览页同一开关。
export function LogsPage() {
  const { t } = useTranslation()
  const [draft, setDraft] = useState<Filter>({ scope: 'key', model: '', errorsOnly: false })
  const [applied, setApplied] = useState<Filter>(draft)

  const commit = (next: Filter) => {
    setDraft(next)
    setApplied(next)
  }

  return (
    <div className="flex flex-col gap-4">
      <PageHeader title={t('logs:title')} description={t('portal:logsDesc')} icon={FileText} />
      <Toolbar
        filters={
          <>
            <div className="flex items-center gap-2">
              <Label>{t('portal:logsScope')}</Label>
              <Segmented
                size="sm"
                value={draft.scope}
                onChange={(s) => commit({ ...draft, scope: s })}
                options={[
                  { value: 'key', label: t('portal:scopeKey') },
                  { value: 'user', label: t('portal:scopeUser') },
                ]}
              />
            </div>
            <SearchInput
              className="w-56"
              value={draft.model}
              placeholder={t('portal:logsModelHint')}
              onChange={(v) => setDraft({ ...draft, model: v })}
              onSubmit={() => setApplied(draft)}
            />
            <Switch
              checked={draft.errorsOnly}
              onChange={(v) => commit({ ...draft, errorsOnly: v })}
              label={t('admin:logsErrorsOnly')}
            />
          </>
        }
        selection={
          <Button size="sm" onClick={() => setApplied(draft)}>
            <Search className="h-3.5 w-3.5" />
            {t('common:search')}
          </Button>
        }
      />
      <LogList filter={applied} />
    </div>
  )
}

function LogList({ filter }: { filter: Filter }) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const [expanded, setExpanded] = useState<number | null>(null)

  // 游标翻页（id 倒序 + before）：账本按 id 单调，游标比 offset 稳——
  // 翻页期间新进的记录不会让后一页和前一页重叠。
  const q = useInfiniteQuery({
    queryKey: qk.logs(params(filter, null)),
    queryFn: ({ pageParam }) => apiFetch<LogsResp>(`/api/me/logs?${params(filter, pageParam)}`),
    initialPageParam: null as number | null,
    getNextPageParam: (last) => (last.data.length < PAGE ? null : last.next_before),
  })

  if (q.isError) {
    return <ErrorState message={describeError(q.error)} onRetry={() => void q.refetch()} />
  }
  if (q.isPending) {
    return <TableSkeleton rows={8} cols={6} />
  }
  const rows = q.data.pages.flatMap((p) => p.data)
  const showKey = filter.scope === 'user'

  return (
    <div className="flex flex-col gap-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <span className="text-xs text-muted-foreground">
          {t('portal:logsLoaded', { n: rows.length })}
        </span>
        <div className="flex gap-2">
          <Button
            size="sm"
            variant="outline"
            disabled={rows.length === 0}
            onClick={() => exportCsv(rows, showKey)}
          >
            <Download className="h-3.5 w-3.5" />
            {t('portal:logsExport')}
          </Button>
          <Button size="sm" variant="outline" loading={q.isRefetching} onClick={() => void q.refetch()}>
            {!q.isRefetching && <RotateCw className="h-3.5 w-3.5" />}
            {t('common:refresh')}
          </Button>
        </div>
      </div>
      {rows.length === 0 ? (
        <EmptyState hint={t('portal:emptyUsageHint')} />
      ) : (
        <Table dense stickyHeader>
          <THead>
            <Tr>
              <Th className="w-6" />
              <Th>{t('logs:time')}</Th>
              <Th>{t('common:status')}</Th>
              {showKey && <Th>{t('portal:keys')}</Th>}
              <Th>{t('pricing:model')}</Th>
              <Th numeric>{t('logs:tokens')}</Th>
              <Th numeric>{t('common:amount')}</Th>
              <Th numeric>{t('admin:logsLatencyTtft')}</Th>
            </Tr>
          </THead>
          <TBody>
            {rows.map((r) => {
              const open = expanded === r.id
              return (
                <Fragment key={r.id}>
                  <Tr
                    className="cursor-pointer"
                    selected={open}
                    aria-expanded={open}
                    onClick={() => setExpanded(open ? null : r.id)}
                  >
                    <Td className="pr-0 text-muted-foreground">
                      <ChevronRight
                        className={cn('h-3.5 w-3.5 transition-transform', open && 'rotate-90')}
                      />
                    </Td>
                    <Td className="whitespace-nowrap text-xs tabular-nums text-muted-foreground">
                      {dayjs(r.created_at).format('MM-DD HH:mm:ss')}
                    </Td>
                    <Td>
                      <Badge dot variant={r.status === 20 ? 'success' : 'destructive'}>
                        {r.status === 20 ? t('logs:ok') : (r.error_code ?? t('logs:failed'))}
                      </Badge>
                    </Td>
                    {showKey && (
                      <Td className="max-w-28 truncate text-xs">
                        {r.key_name || (r.api_key_id !== null ? `#${r.api_key_id}` : '—')}
                      </Td>
                    )}
                    <Td className="whitespace-nowrap font-mono text-xs">{r.model}</Td>
                    <Td numeric className="whitespace-nowrap text-xs">
                      {r.usage.prompt_tokens}
                      {r.usage.cached_tokens > 0 && (
                        <span className="text-muted-foreground">
                          ({t('logs:cachedShort', { n: r.usage.cached_tokens })})
                        </span>
                      )}
                      {' + '}
                      {r.usage.completion_tokens}
                    </Td>
                    <Td numeric className="font-medium">
                      {formatMoney(r.amount_micro, locale)}
                    </Td>
                    <Td numeric className="whitespace-nowrap font-mono text-xs">
                      {r.latency_ms === null ? (
                        '—'
                      ) : (
                        <>
                          {r.latency_ms}
                          {r.is_stream && r.ttft_ms !== null && (
                            <span className="text-muted-foreground"> / {r.ttft_ms}</span>
                          )}
                          ms
                        </>
                      )}
                    </Td>
                  </Tr>
                  {open && (
                    <Tr className="hover:bg-transparent">
                      <Td colSpan={showKey ? 8 : 7} className="bg-muted/30 p-0">
                        <BillExplainer row={r} locale={locale} />
                      </Td>
                    </Tr>
                  )}
                </Fragment>
              )
            })}
          </TBody>
        </Table>
      )}
      {q.hasNextPage && (
        <Button
          variant="outline"
          className="self-center"
          disabled={q.isFetchingNextPage}
          onClick={() => void q.fetchNextPage()}
        >
          {q.isFetchingNextPage ? t('common:loading') : t('portal:logsLoadMore')}
        </Button>
      )}
    </div>
  )
}

/// 导出已加载的行：合作商给员工分摊账单、财务对账，要的都是"拿到表格"。
function exportCsv(rows: LogRow[], withKey: boolean) {
  downloadCsv(
    'okapi-usage',
    [
      'time',
      'status',
      ...(withKey ? ['key'] : []),
      'model',
      'prompt_tokens',
      'cached_tokens',
      'completion_tokens',
      'reasoning_tokens',
      'amount_usd',
      'original_usd',
      'discount_usd',
      'latency_ms',
      'ttft_ms',
      'request_id',
    ],
    rows.map((r) => [
      r.created_at,
      r.status === 20 ? 'ok' : (r.error_code ?? 'failed'),
      ...(withKey ? [r.key_name || r.api_key_id] : []),
      r.model,
      r.usage.prompt_tokens,
      r.usage.cached_tokens,
      r.usage.completion_tokens,
      r.usage.reasoning_tokens,
      microToUsd(r.amount_micro),
      microToUsd(r.original_amount_micro),
      microToUsd(r.discount_micro),
      r.latency_ms,
      r.ttft_ms,
      r.request_id,
    ]),
  )
}

/// 账单解释器：吃 pricing_snapshot 逐层展开（DESIGN §3：snapshot 是计费唯一语义）。
///
/// 左列是"钱"（原价 → 优惠 → 实扣），右列是"为什么是这个数"（倍率与规则链）；
/// 请求 ID 单独一行带复制——工单/退款都以它为锚，手动框选 UUID 极易漏字符。
function BillExplainer({ row, locale }: { row: LogRow; locale: string }) {
  const { t } = useTranslation()
  const s = row.pricing_snapshot
  const money = (label: string, value: string, cls?: string) => (
    <div className="flex items-baseline justify-between gap-4">
      <span className="text-muted-foreground">{label}</span>
      <span className={cn('font-medium tabular-nums', cls)}>{value}</span>
    </div>
  )
  return (
    <div className="grid gap-4 px-4 py-3 text-xs md:grid-cols-[minmax(0,1fr)_minmax(0,2fr)] animate-fade-in">
      <div className="flex flex-col gap-1.5 rounded-md border border-border bg-card p-3">
        {money(t('logs:original'), formatMoney(row.original_amount_micro, locale))}
        {row.discount_micro > 0 &&
          money(t('logs:discount'), `-${formatMoney(row.discount_micro, locale)}`, 'text-success')}
        <div className="my-0.5 h-px bg-border" />
        {money(t('logs:final'), formatMoney(row.amount_micro, locale), 'text-sm')}
        {row.usage.reasoning_tokens > 0 &&
          money(t('admin:logsReasoning'), String(row.usage.reasoning_tokens), 'text-muted-foreground')}
      </div>
      <div className="flex min-w-0 flex-col gap-2.5">
        {s ? (
          <div className="flex flex-wrap gap-1.5">
            <Badge variant="muted">
              {t('logs:mode')} {s.mode}
            </Badge>
            {s.model_ratio !== null && (
              <Badge variant="muted">
                {t('admin:modelRatio')} ×{s.model_ratio}
              </Badge>
            )}
            {s.completion_ratio !== null && (
              <Badge variant="muted">
                {t('admin:completionRatio')} ×{s.completion_ratio}
              </Badge>
            )}
            {s.cache_ratio !== null && row.usage.cached_tokens > 0 && (
              <Badge variant="muted">
                {t('admin:cacheRatio')} ×{s.cache_ratio}
              </Badge>
            )}
            <Badge variant="muted">
              {t('logs:group')} {s.group} ×{s.group_ratio}
            </Badge>
            {s.user_multiplier !== '1' && (
              <Badge variant="muted">
                {t('logs:userMultiplier')} ×{s.user_multiplier}
              </Badge>
            )}
            {s.rules.map((rule) => (
              <Badge key={rule.code}>
                {rule.code} ×{rule.multiplier}
              </Badge>
            ))}
          </div>
        ) : (
          <p className="text-muted-foreground">{t('logs:noSnapshot')}</p>
        )}
        <div className="flex items-center gap-2 text-muted-foreground">
          <span>{t('admin:logsRequestId')}</span>
          <CopyText value={row.request_id} className="min-w-0 text-foreground" />
        </div>
      </div>
    </div>
  )
}
