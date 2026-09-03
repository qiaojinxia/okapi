import { useInfiniteQuery, useQuery } from '@tanstack/react-query'
import { getRouteApi, useNavigate } from '@tanstack/react-router'
import dayjs from 'dayjs'
import { ChevronDown, ChevronRight, ScrollText } from 'lucide-react'
import { Fragment, useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { AuditSearch } from '@/routes/admin.audit'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Input, Label } from '@/components/ui/input'
import { PageHeader, Toolbar } from '@/components/ui/page'
import { Segmented } from '@/components/ui/segmented'
import { Select } from '@/components/ui/select'
import { TableSkeleton } from '@/components/ui/skeleton'
import { EmptyState, ErrorState } from '@/components/ui/state'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { qk } from '@/lib/query-keys'
import { cn } from '@/lib/utils'

const routeApi = getRouteApi('/admin/audit')
const DEFAULT_HOURS = 168
const HOURS = [24, 168, 720, 2160] as const
const PAGE = 50

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

/// detail 一层展开为键值行；嵌套值压成紧凑 JSON——审计详情是给人核对的，
/// 整块 JSON 文本框要用户自己在花括号里找字段。
function detailEntries(detail: Record<string, unknown> | null): [string, string][] {
  if (!detail) return []
  return Object.entries(detail)
    .filter(([, v]) => v !== null && v !== undefined && v !== '')
    .map(([k, v]) => [k, typeof v === 'object' ? JSON.stringify(v) : String(v)])
}

/// 审计日志页：谁在何时改了什么（含登录记录）。
///
/// 过滤走草稿 / 提交两态（与日志页同法），已提交态 = URL；detail 缺省只露前两个键，
/// 点行展开全部——一屏先看得到"谁 / 做了什么 / 对谁"，细节按需。翻页用游标，
/// 审计表只增，翻页期间新写入不会让两页重叠。
export function AuditPage() {
  const { t } = useTranslation()
  const search = routeApi.useSearch()
  const navigate = useNavigate({ from: '/admin/audit' })
  const [draft, setDraft] = useState<Draft>(() => fromSearch(search))
  const [open, setOpen] = useState<Set<number>>(new Set())
  useEffect(() => setDraft(fromSearch(search)), [search])

  const submit = (d: Draft) => void navigate({ search: toSearch(d) })

  const actions = useQuery({
    queryKey: qk.auditActions,
    queryFn: () => apiFetch<{ data: string[] }>('/admin/audit/actions'),
    staleTime: 300_000,
  })
  const q = useInfiniteQuery({
    queryKey: qk.audit(toParams(search)),
    queryFn: ({ pageParam }) =>
      apiFetch<AuditResp>(`/admin/audit?${toParams(search, pageParam as number | undefined)}`),
    initialPageParam: undefined as number | undefined,
    getNextPageParam: (last) => (last.has_more ? (last.next_before ?? undefined) : undefined),
  })
  const rows = q.data?.pages.flatMap((p) => p.data) ?? []

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
    <div className="flex flex-col gap-4">
      <PageHeader title={t('admin:auditTitle')} description={t('admin:auditDesc')} icon={ScrollText} />

      <Toolbar
        filters={
          <>
            <div className="flex flex-col gap-1.5">
              <Label htmlFor="au-action">{t('admin:auditAction')}</Label>
              <Select
                id="au-action"
                className="w-56"
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
            <div className="flex flex-col gap-1.5">
              <Label htmlFor="au-target">{t('admin:auditTarget')}</Label>
              <Input
                id="au-target"
                className="w-48"
                value={draft.target}
                placeholder={t('admin:auditTargetHint')}
                onChange={(e) => setDraft((d) => ({ ...d, target: e.target.value }))}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') submit(draft)
                }}
              />
            </div>
            <div className="flex flex-col gap-1.5">
              <Label htmlFor="au-actor">{t('admin:auditActor')}</Label>
              <Input
                id="au-actor"
                className="w-36 font-mono"
                value={draft.actor}
                placeholder="admin:42"
                onChange={(e) => setDraft((d) => ({ ...d, actor: e.target.value }))}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') submit(draft)
                }}
              />
            </div>
            <Segmented
              options={HOURS.map((h) => ({ value: h, label: hoursLabel(h) }))}
              value={draft.hours}
              onChange={(h) => submit({ ...draft, hours: h })}
              size="sm"
            />
            <Button size="sm" onClick={() => submit(draft)}>
              {t('common:search')}
            </Button>
          </>
        }
        selection={
          <span className="text-xs text-muted-foreground">
            {t('admin:auditLoaded', { n: rows.length })}
          </span>
        }
      />

      {q.isError ? (
        <ErrorState message={describeError(q.error)} onRetry={() => void q.refetch()} />
      ) : q.isPending ? (
        <TableSkeleton rows={8} cols={6} />
      ) : rows.length === 0 ? (
        <EmptyState hint={t('admin:auditEmptyHint')} />
      ) : (
        <Table>
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
              const expanded = open.has(r.id)
              const ip = r.ip ?? (typeof r.detail?.ip === 'string' ? r.detail.ip : null)
              return (
                <Fragment key={r.id}>
                  <Tr
                    className={cn(entries.length > 0 && 'cursor-pointer')}
                    onClick={() =>
                      setOpen((prev) => {
                        const next = new Set(prev)
                        if (next.has(r.id)) next.delete(r.id)
                        else next.add(r.id)
                        return next
                      })
                    }
                  >
                    <Td className="text-muted-foreground">
                      {entries.length > 0 &&
                        (expanded ? (
                          <ChevronDown className="h-3.5 w-3.5" />
                        ) : (
                          <ChevronRight className="h-3.5 w-3.5" />
                        ))}
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
                    {/* 摘要必须真截断：UA 之类的长值会把 IP 列挤出屏幕；完整值在展开区 */}
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
                  {expanded && entries.length > 0 && (
                    <Tr>
                      <Td />
                      <Td colSpan={6} className="bg-muted/30">
                        <dl className="grid grid-cols-[max-content_1fr] gap-x-4 gap-y-1 py-1 text-xs">
                          {entries.map(([k, v]) => (
                            <Fragment key={k}>
                              <dt className="font-mono text-muted-foreground">{k}</dt>
                              <dd className="font-mono break-all">{v}</dd>
                            </Fragment>
                          ))}
                        </dl>
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
          size="sm"
          className="self-center"
          disabled={q.isFetchingNextPage}
          onClick={() => void q.fetchNextPage()}
        >
          {t('common:loadMore')}
        </Button>
      )}
    </div>
  )
}
