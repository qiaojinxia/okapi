import { useInfiniteQuery, useQuery } from '@tanstack/react-query'
import { getRouteApi, useNavigate } from '@tanstack/react-router'
import dayjs from 'dayjs'
import { ScrollText } from 'lucide-react'
import { useEffect, useId, useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { AuditSearch } from '@/routes/admin.audit'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { CopyButton } from '@/components/ui/copy-button'
import { Drawer, FieldGroup } from '@/components/ui/drawer'
import { Input, Label } from '@/components/ui/input'
import { PageHeader, Toolbar } from '@/components/ui/page'
import { RowExpander } from '@/components/ui/row-expander'
import { Segmented } from '@/components/ui/segmented'
import { Select } from '@/components/ui/select'
import { TableSkeleton } from '@/components/ui/skeleton'
import { EmptyState, ErrorState } from '@/components/ui/state'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { DEFAULT_PAGE_SIZE } from '@/hooks/use-pagination'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { qk } from '@/lib/query-keys'

const routeApi = getRouteApi('/admin/audit')
const DEFAULT_HOURS = 168
const HOURS = [24, 168, 720, 2160] as const
const PAGE = DEFAULT_PAGE_SIZE

interface AuditRow {
  id: number
  actor: string
  actor_info: { kind: string; id: number | null; label: string | null } | null
  action: string
  target: string | null
  detail: Record<string, unknown> | null
  ip: string | null
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
  hours: number
}

function fromSearch(s: AuditSearch): Draft {
  return {
    actor: s.actor ?? '',
    action: s.action ?? '',
    target: s.target ?? '',
    hours: s.hours ?? DEFAULT_HOURS,
  }
}

function toSearch(d: Draft): AuditSearch {
  return {
    actor: d.actor.trim() || undefined,
    action: d.action.trim() || undefined,
    target: d.target.trim() || undefined,
    hours: d.hours === DEFAULT_HOURS ? undefined : d.hours,
  }
}

function toParams(s: AuditSearch, before?: number): string {
  const p = new URLSearchParams({ limit: String(PAGE), hours: String(s.hours ?? DEFAULT_HOURS) })
  if (s.actor) p.set('actor', s.actor)
  if (s.action) p.set('action', s.action)
  if (s.target) p.set('target', s.target)
  if (before !== undefined) p.set('before', String(before))
  return p.toString()
}

/// 动作着色：删除 / 丢弃 / 停用 / 登录失败是红黄类，其余中性。
/// 按动作后缀判断而不是维护清单——动作名会随功能增长，清单必然漏。
function actionTone(action: string): 'destructive' | 'warning' | 'muted' | 'default' {
  const verb = action.split('.').pop() ?? ''
  if (/^(delete|discard|revoke|ban)/.test(verb)) return 'destructive'
  if (/failed|disable|refund|flush/.test(verb)) return 'warning'
  if (/^(create|upsert|publish|credit|set_)/.test(verb)) return 'default'
  return 'muted'
}

/// detail 按键展示；列表使用紧凑摘要，抽屉缩进嵌套 JSON 便于核对。
function detailEntries(detail: Record<string, unknown> | null, pretty = false): [string, string][] {
  if (!detail) return []
  return Object.entries(detail)
    .filter(([, v]) => v !== null && v !== undefined && v !== '')
    .map(([k, v]) => [k, typeof v === 'object' ? JSON.stringify(v, null, pretty ? 2 : undefined) : String(v)])
}

/// 审计日志页：谁在何时改了什么（含登录记录）。
///
/// 过滤走草稿 / 提交两态（与日志页同法），已提交态 = URL；detail 缺省只露前两个键，
/// 点行打开右侧详情抽屉，一屏先看得到"谁 / 做了什么 / 对谁"。翻页用游标，
/// 审计表只增，翻页期间新写入不会让两页重叠。
export function AuditPage() {
  const { t } = useTranslation()
  const search = routeApi.useSearch()
  const navigate = useNavigate({ from: '/admin/audit' })
  const [draft, setDraft] = useState<Draft>(() => fromSearch(search))
  const [selected, setSelected] = useState<number | null>(null)
  const detailId = useId(), params = toParams(search)
  useEffect(() => setDraft(fromSearch(search)), [search])
  useEffect(() => setSelected(null), [params])

  const submit = (d: Draft) => void navigate({ search: toSearch(d) })

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
      default:
        return ''
    }
  }
  const hoursLabel = (h: number) =>
    h < 168 ? t('admin:auditHours', { n: h }) : t('admin:lastDays', { days: h / 24 })

  return (
    <div className="list-page">
      <PageHeader title={t('admin:auditTitle')} description={t('admin:auditDesc')} icon={ScrollText} />

      <Toolbar
        filtersClassName="items-end"
        selectionClassName="min-h-9 self-end"
        filters={
          <>
            <div className="flex w-full min-w-0 flex-col gap-1.5 sm:w-56">
              <Label htmlFor="au-action">{t('admin:auditAction')}</Label>
              <Select
                id="au-action"
                className="w-full"
                value={draft.action}
                onChange={(v) => setDraft((d) => ({ ...d, action: v }))}
                placeholder={t('common:all')}
                options={[
                  // 前缀档：一类动作一起看（用户类 / 渠道类 / 定价类）
                  ...['channel.', 'pricing.', 'user.', 'billing.', 'settings.'].map((p) => ({
                    value: p,
                    label: t('admin:auditActionGroup', { prefix: p }),
                  })),
                  ...(actions.data?.data ?? []).map((a) => ({ value: a, label: a })),
                ]}
              />
            </div>
            <div className="flex w-full min-w-0 flex-col gap-1.5 sm:w-64">
              <Label htmlFor="au-target">{t('admin:auditTarget')}</Label>
              <Input
                id="au-target"
                className="w-full"
                value={draft.target}
                placeholder={t('admin:auditTargetHint')}
                onChange={(e) => setDraft((d) => ({ ...d, target: e.target.value }))}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') submit(draft)
                }}
              />
            </div>
            <div className="flex w-full min-w-0 flex-col gap-1.5 sm:w-40">
              <Label htmlFor="au-actor">{t('admin:auditActor')}</Label>
              <Input
                id="au-actor"
                className="w-full font-mono"
                value={draft.actor}
                placeholder="admin:42"
                onChange={(e) => setDraft((d) => ({ ...d, actor: e.target.value }))}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') submit(draft)
                }}
              />
            </div>
            <div className="flex min-w-0 max-w-full flex-wrap items-center gap-2">
              <Segmented
                options={HOURS.map((h) => ({ value: h, label: hoursLabel(h) }))}
                value={draft.hours}
                onChange={(h) => submit({ ...draft, hours: h })}
                size="sm"
                ariaLabel={t('admin:logsRange')}
                className="md:h-9"
              />
              <Button size="sm" variant="outline" onClick={() => submit(draft)}>
                {t('common:search')}
              </Button>
            </div>
          </>
        }
        selection={
          <Badge variant="muted" className="tabular-nums">
            {t('admin:auditLoaded', { n: rows.length })}
          </Badge>
        }
      />

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
              const entries = detailEntries(r.detail)
              const expanded = selected === r.id
              const ip = r.ip ?? (typeof r.detail?.ip === 'string' ? r.detail.ip : null)
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
                    <Td className="whitespace-nowrap text-xs text-muted-foreground">
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
                    {/* 长值只在抽屉中展示，摘要不把 IP 列挤出屏幕。 */}
                    <Td className="max-w-56 text-xs text-muted-foreground">
                      {entries.length === 0 ? (
                        '—'
                      ) : (
                        <span className="block max-w-56 truncate">
                          {entries
                            .slice(0, 2)
                            .map(([k, v]) => `${k}=${v}`)
                            .join(' · ')}
                          {entries.length > 2 ? ` · +${entries.length - 2}` : ''}
                        </span>
                      )}
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
        row={selectedRow} id={detailId} onClose={() => setSelected(null)}
        actor={selectedRow ? actorLabel(selectedRow) : ''}
        kind={selectedRow ? actorKind(selectedRow) : ''}
      />
    </div>
  )
}

function AuditDetail({ row, id, onClose, actor, kind }: {
  row: AuditRow | null; id: string; onClose: () => void; actor: string; kind: string
}) {
  const { t } = useTranslation()
  if (!row) return null
  const entries = detailEntries(row.detail, true)
  const ip = row.ip ?? (typeof row.detail?.ip === 'string' ? row.detail.ip : null)
  const field = (label: string, content: React.ReactNode) => <div className="min-w-0 space-y-1">
    <dt className="text-xs text-muted-foreground">{label}</dt>
    <dd className="break-words text-sm">{content}</dd>
  </div>
  return <Drawer open onClose={onClose} title={t('admin:auditDetailTitle')} description={t('admin:auditDetailHint')} size="lg">
    <div id={id}>
      <FieldGroup title={t('admin:auditRecordInfo')}>
        <dl className="grid grid-cols-2 gap-x-5 gap-y-4">
          {field(t('admin:auditRecordId'), <AuditValue value={String(row.id)} />)}
          {field(t('admin:auditTime'), dayjs(row.created_at).format('YYYY-MM-DD HH:mm:ss'))}
          {field(t('admin:auditActor'), <div className="space-y-1">
            <p className="break-all">{actor}{kind && <Badge variant="muted" className="ml-2">{kind}</Badge>}</p>
            {row.actor !== 'anon' && <AuditValue value={row.actor} />}
          </div>)}
          {field(t('admin:auditAction'), <Badge variant={actionTone(row.action)} className="max-w-full break-all whitespace-normal font-mono">{row.action}</Badge>)}
          {field(t('admin:auditTarget'), row.target ? <AuditValue value={row.target} /> : '—')}
          {field('IP', ip ? <AuditValue value={ip} /> : '—')}
        </dl>
      </FieldGroup>
      <FieldGroup title={t('admin:auditDetail')}>
        {entries.length ? <dl className="space-y-4">{entries.map(([key, value]) => <div key={key} className="min-w-0 space-y-1.5">
          <dt className="break-all font-mono text-xs text-muted-foreground">{key}</dt>
          <dd className="rounded-lg bg-muted/50 p-3"><AuditValue value={value} /></dd>
        </div>)}</dl> : <p className="text-sm text-muted-foreground">—</p>}
      </FieldGroup>
    </div>
  </Drawer>
}

function AuditValue({ value }: { value: string }) {
  return <span className="flex min-w-0 items-start gap-2">
    <span className="min-w-0 flex-1 whitespace-pre-wrap break-all font-mono text-xs leading-6">{value}</span>
    <CopyButton value={value} size="xs" />
  </span>
}
