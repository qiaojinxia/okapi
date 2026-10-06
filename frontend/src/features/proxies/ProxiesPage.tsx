import { keepPreviousData, useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { Activity, Globe, ListTree, Pencil, Plus, Settings, Trash2, Upload } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button, buttonVariants } from '@/components/ui/button'
import { useConfirm } from '@/components/ui/confirm'
import { IconButton } from '@/components/ui/icon-button'
import { PageHeader } from '@/components/ui/page'
import { Pagination } from '@/components/ui/pagination'
import { TableSkeleton } from '@/components/ui/skeleton'
import { EmptyState, ErrorState } from '@/components/ui/state'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { Tabs } from '@/components/ui/tabs'
import { toast } from '@/components/ui/toast'
import { usePagination } from '@/hooks/use-pagination'
import { usePermission } from '@/hooks/use-auth'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { qk } from '@/lib/query-keys'
import { AssignmentsDrawer } from './AssignmentsDrawer'
import { GroupDrawer } from './GroupDrawer'
import { ImportDrawer } from './ImportDrawer'
import { ProxyDrawer } from './ProxyDrawer'
import { useReportToast } from './report'
import type { ProbeResult, ProxyGroupRow, ProxyRow } from './types'
import { GROUP_MODE_LABEL, exitIpRecentlyChanged } from './types'

type Tab = 'proxies' | 'groups'

/// 出口代理页（IMPLEMENTATION §11.41）。
///
/// 代理是一等资源：渠道按「继承全局默认 / 直连 / 单个代理 / 代理组」绑定出口（在渠道抽屉里选），
/// 不挂在渠道池上——一个渠道可以同时在多个池里，按池绑出口会让同一个账号从多个 IP 出去。
/// 本页管代理本身与代理组；全局默认出口和后台探测是站点级配置，在系统设置的「出口代理」页签。
export function ProxiesPage() {
  const { t } = useTranslation()
  const can = usePermission()
  const [tab, setTab] = useState<Tab>('proxies')
  const tabs = (
    <Tabs
      ariaLabel={t('admin:proxiesTitle')}
      items={[
        { id: 'proxies', label: t('admin:proxiesTab') },
        { id: 'groups', label: t('admin:proxyGroupsTab') },
      ]}
      active={tab}
      onChange={(id) => setTab(id as Tab)}
    />
  )
  return (
    <div className="list-page">
      <PageHeader
        icon={Globe}
        title={t('admin:proxiesTitle')}
        description={t('admin:proxiesDesc')}
        action={can('settings.read') ? (
          <Link to="/admin/settings" search={{ tab: 'egress' }} className={buttonVariants({ variant: 'outline' })}>
            <Settings aria-hidden className="h-4 w-4" />
            {t('admin:egressSettingsLink')}
          </Link>
        ) : undefined}
      />
      {tab === 'proxies' ? <ProxyList tabs={tabs} /> : <GroupList tabs={tabs} />}
    </div>
  )
}

/// 最近一次失败的原因：网关记的连接失败码、核实 / 测试用的探测码翻成人话，其余（测试失败的原文）照原样。
function failureText(t: (key: string, options?: Record<string, unknown>) => string, raw: string | null) {
  if (raw === null) return undefined
  return t(`admin:proxyFailure_${raw}`, { defaultValue: t(`admin:proxyProbe_${raw}`, { defaultValue: raw }) })
}

function ProxyStatus({ proxy }: { proxy: ProxyRow }) {
  const { t } = useTranslation()
  if (proxy.status !== 1) return <Badge variant="muted">{t('common:disabled')}</Badge>
  if (proxy.cooling) {
    return (
      <Badge variant="warning" title={failureText(t, proxy.last_error)}>
        {t('admin:proxyCooling')}
      </Badge>
    )
  }
  return <Badge variant="success" dot>{t('common:enabled')}</Badge>
}

/// 页签、条数与操作按钮同一行，表格紧跟其后（页头下不再单独占两行）。
function ListToolbar({ tabs, total, children }: { tabs: React.ReactNode; total: number; children: React.ReactNode }) {
  const { t } = useTranslation()
  return (
    <div className="flex flex-wrap items-center justify-between gap-2">
      <span className="flex items-center gap-3">
        {tabs}
        <Badge variant="muted">{t('admin:keyTotal', { n: total })}</Badge>
      </span>
      <span className="flex items-center gap-2">{children}</span>
    </div>
  )
}

function ProxyList({ tabs }: { tabs: React.ReactNode }) {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const pager = usePagination()
  const { confirm, dialog } = useConfirm()
  const [drawer, setDrawer] = useState<{ proxy?: ProxyRow } | null>(null)
  const [importing, setImporting] = useState(false)
  const report = useReportToast()
  const list = useQuery({
    queryKey: [...qk.adminProxies, pager.offset, pager.limit],
    queryFn: () =>
      apiFetch<{ data: ProxyRow[]; total: number }>(
        `/admin/proxies?limit=${pager.limit}&offset=${pager.offset}`,
      ),
    placeholderData: keepPreviousData,
  })
  const invalidate = () => {
    void queryClient.invalidateQueries({ queryKey: qk.adminProxies })
    void queryClient.invalidateQueries({ queryKey: qk.adminProxyGroups })
  }
  const test = useMutation({
    mutationFn: (id: number) => apiFetch<ProbeResult>(`/admin/proxies/${id}/test`, { method: 'POST' }),
    onSuccess: (r) => {
      if (r.ok && r.exit_ip_changed) {
        // 出口 IP 变了：固定分配在它上面的账号都随之换了 IP，用警告而不是成功提示
        toast.warning(t('admin:proxyExitChangedToast', r.exit_ip_changed))
      } else if (r.ok) {
        toast.success(
          t('admin:proxyProbeOkToast', {
            ip: r.exit_ip ?? t('admin:proxyExitUnknown'),
            ms: r.latency_ms ?? 0,
          }),
        )
      } else {
        toast.error(
          `${t(`admin:proxyProbe_${r.error_code ?? 'probe_failed'}`, { defaultValue: r.error_code ?? '' })}${r.error ? `：${r.error}` : ''}`,
        )
      }
      invalidate()
    },
    onError: (err) => toast.error(describeError(err)),
  })
  const remove = useMutation({
    mutationFn: (id: number) => apiFetch(`/admin/proxies/${id}`, { method: 'DELETE' }),
    onSuccess: () => {
      toast.success(t('common:success'))
      invalidate()
    },
    onError: (err) => toast.error(describeError(err)),
  })
  const rows = list.data?.data ?? []
  const create = (
    <Button onClick={() => setDrawer({})}>
      <Plus className="h-4 w-4" />
      {t('admin:proxyCreate')}
    </Button>
  )

  return (
    <>
      {dialog}
      <ListToolbar tabs={tabs} total={list.data?.total ?? 0}>
        <Button variant="outline" onClick={() => setImporting(true)}>
          <Upload className="h-4 w-4" />
          {t('admin:proxyImport')}
        </Button>
        {create}
      </ListToolbar>
      {list.isError ? (
        <ErrorState message={describeError(list.error)} onRetry={() => void list.refetch()} />
      ) : list.isPending ? (
        <TableSkeleton rows={6} cols={6} />
      ) : rows.length === 0 ? (
        <EmptyState hint={t('admin:proxiesEmptyHint')} action={create} />
      ) : (
        <Table stickyHeader>
          <THead>
            <Tr>
              <Th>{t('admin:proxyName')}</Th>
              <Th>{t('common:status')}</Th>
              <Th>{t('admin:proxyExit')}</Th>
              <Th>{t('admin:proxyUsage')}</Th>
              <Th className="text-right">{t('common:actions')}</Th>
            </Tr>
          </THead>
          <TBody>
            {rows.map((p) => {
              const used = p.channel_count > 0 || p.is_default
              return (
                <Tr key={p.id}>
                  <Td>
                    <span className="flex max-w-72 flex-col">
                      <span className="flex items-center gap-1.5">
                        <span className="truncate font-medium" title={p.name}>{p.name}</span>
                        {p.is_default && <Badge variant="info">{t('admin:egressDefaultBadge')}</Badge>}
                      </span>
                      <span className="truncate font-mono text-xs text-muted-foreground" title={p.url_masked}>
                        {p.url_masked}
                      </span>
                      {p.note && <span className="truncate text-xs text-muted-foreground">{p.note}</span>}
                    </span>
                  </Td>
                  <Td>
                    <span className="flex flex-col items-start gap-1">
                      <ProxyStatus proxy={p} />
                      {p.failed_count > 0 && !p.cooling && (
                        <span className="text-xs text-muted-foreground">
                          {t('admin:proxyRecentFailures', { n: p.failed_count })}
                        </span>
                      )}
                    </span>
                  </Td>
                  <Td>
                    {p.checked_at === null ? (
                      <span className="text-xs text-muted-foreground">{t('admin:proxyNeverTested')}</span>
                    ) : (
                      <span className="flex flex-col text-xs">
                        <span className="flex items-center gap-1.5">
                          <span className="font-mono">{p.exit_ip ?? '—'}</span>
                          {p.exit_country && <Badge variant="outline">{p.exit_country}</Badge>}
                          {exitIpRecentlyChanged(p) && (
                            <Badge
                              variant="warning"
                              title={t('admin:proxyExitChangedTip', {
                                previous: p.previous_exit_ip ?? '—',
                                current: p.exit_ip ?? '—',
                                at: p.exit_ip_changed_at ? new Date(p.exit_ip_changed_at).toLocaleString() : '',
                              })}
                            >
                              {t('admin:proxyExitChanged')}
                            </Badge>
                          )}
                        </span>
                        {p.last_error && !p.cooling && (
                          <span className="truncate text-destructive" title={failureText(t, p.last_error)}>
                            {t('admin:proxyLastProbeFailed')}
                          </span>
                        )}
                        <span className="text-muted-foreground tabular-nums">
                          {p.latency_ms !== null ? `${p.latency_ms} ms · ` : ''}
                          {new Date(p.checked_at).toLocaleString()}
                        </span>
                      </span>
                    )}
                  </Td>
                  <Td className="text-xs">
                    <span className="flex flex-col">
                      <span>{t('admin:proxyUsageChannels', { n: p.channel_count })}</span>
                      <span className="text-muted-foreground">
                        {t('admin:proxyUsageKeys', { n: p.assigned_keys })}
                        {p.max_keys !== null ? ` / ${p.max_keys}` : ''}
                      </span>
                      {p.max_concurrency !== null && (
                        <span className="text-muted-foreground">
                          {t('admin:proxyUsageConcurrency', { n: p.max_concurrency })}
                        </span>
                      )}
                      {p.groups.length > 0 && (
                        <span className="truncate text-muted-foreground" title={p.groups.join(', ')}>
                          {t('admin:proxyUsageGroups', { groups: p.groups.join(', ') })}
                        </span>
                      )}
                    </span>
                  </Td>
                  <Td>
                    <div className="flex items-center justify-end gap-0.5">
                      <IconButton
                        icon={Activity}
                        label={t('admin:proxyTest')}
                        disabled={test.isPending}
                        onClick={() => test.mutate(p.id)}
                      />
                      <IconButton icon={Pencil} label={t('common:edit')} onClick={() => setDrawer({ proxy: p })} />
                      <IconButton
                        icon={Trash2}
                        label={t('common:delete')}
                        variant="destructive"
                        disabled={used}
                        onClick={() =>
                          confirm({
                            title: t('common:confirmDeleteTitle', { name: p.name }),
                            description: t(
                              p.assigned_keys > 0 ? 'admin:proxyDeleteReassignConfirm' : 'admin:proxyDeleteConfirm',
                              { n: p.assigned_keys },
                            ),
                            onConfirm: () => remove.mutate(p.id),
                          })
                        }
                      />
                    </div>
                  </Td>
                </Tr>
              )
            })}
          </TBody>
        </Table>
      )}
      <Pagination {...pager} total={list.data?.total} />
      {drawer !== null && (
        <ProxyDrawer
          proxy={drawer.proxy}
          onClose={() => setDrawer(null)}
          onDone={(r) => {
            report(r)
            invalidate()
          }}
        />
      )}
      {importing && <ImportDrawer onClose={() => setImporting(false)} onDone={invalidate} />}
    </>
  )
}

function GroupList({ tabs }: { tabs: React.ReactNode }) {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const pager = usePagination()
  const { confirm, dialog } = useConfirm()
  const [drawer, setDrawer] = useState<{ group?: ProxyGroupRow } | null>(null)
  const [assignments, setAssignments] = useState<ProxyGroupRow | null>(null)
  const report = useReportToast()
  const list = useQuery({
    queryKey: [...qk.adminProxyGroups, pager.offset, pager.limit],
    queryFn: () =>
      apiFetch<{ data: ProxyGroupRow[]; total: number }>(
        `/admin/proxy-groups?limit=${pager.limit}&offset=${pager.offset}`,
      ),
    placeholderData: keepPreviousData,
  })
  const invalidate = () => {
    void queryClient.invalidateQueries({ queryKey: qk.adminProxyGroups })
    void queryClient.invalidateQueries({ queryKey: qk.adminProxies })
  }
  const remove = useMutation({
    mutationFn: (code: string) =>
      apiFetch(`/admin/proxy-groups/${encodeURIComponent(code)}`, { method: 'DELETE' }),
    onSuccess: () => {
      toast.success(t('common:success'))
      invalidate()
    },
    onError: (err) => toast.error(describeError(err)),
  })
  const rows = list.data?.data ?? []
  const create = (
    <Button onClick={() => setDrawer({})}>
      <Plus className="h-4 w-4" />
      {t('admin:proxyGroupCreate')}
    </Button>
  )

  return (
    <>
      {dialog}
      <ListToolbar tabs={tabs} total={list.data?.total ?? 0}>
        {create}
      </ListToolbar>
      {list.isError ? (
        <ErrorState message={describeError(list.error)} onRetry={() => void list.refetch()} />
      ) : list.isPending ? (
        <TableSkeleton rows={4} cols={5} />
      ) : rows.length === 0 ? (
        <EmptyState hint={t('admin:proxyGroupsEmptyHint')} action={create} />
      ) : (
        <Table stickyHeader>
          <THead>
            <Tr>
              <Th>{t('admin:proxyGroupCode')}</Th>
              <Th>{t('admin:proxyGroupMode')}</Th>
              <Th>{t('admin:proxyGroupMembers')}</Th>
              <Th>{t('admin:proxyUsage')}</Th>
              <Th className="text-right">{t('common:actions')}</Th>
            </Tr>
          </THead>
          <TBody>
            {rows.map((g) => (
              <Tr key={g.code}>
                <Td>
                  <span className="flex flex-col">
                    <span className="flex items-center gap-1.5">
                      <span className="font-medium">{g.name}</span>
                      {g.is_default && <Badge variant="info">{t('admin:egressDefaultBadge')}</Badge>}
                    </span>
                    <span className="font-mono text-xs text-muted-foreground">{g.code}</span>
                  </span>
                </Td>
                <Td>
                  <Badge variant={g.mode === 'pinned' ? 'default' : 'muted'}>{t(GROUP_MODE_LABEL[g.mode])}</Badge>
                </Td>
                <Td>
                  <span className="flex max-w-80 flex-wrap gap-1">
                    {g.members.length === 0 ? (
                      <Badge variant="destructive">{t('admin:proxyGroupNoMembers')}</Badge>
                    ) : (
                      g.members.map((m) => (
                        <Badge
                          key={m.proxy_id}
                          variant={m.status !== 1 ? 'muted' : m.cooling ? 'warning' : 'outline'}
                          title={g.mode === 'pinned' ? t('admin:proxyUsageKeys', { n: m.assigned_keys }) : undefined}
                        >
                          {m.name}
                          {g.mode === 'pinned' ? ` · ${m.assigned_keys}` : ''}
                        </Badge>
                      ))
                    )}
                  </span>
                </Td>
                <Td className="text-xs">
                  <span className="flex flex-col">
                    <span>{t('admin:proxyUsageChannels', { n: g.channel_count })}</span>
                    {g.unassigned_keys > 0 && (
                      <span className="text-warning">{t('admin:proxyGroupUnassigned', { n: g.unassigned_keys })}</span>
                    )}
                  </span>
                </Td>
                <Td>
                  <div className="flex items-center justify-end gap-0.5">
                    <IconButton icon={ListTree} label={t('admin:proxyAssignmentsTitle', { name: g.name })} onClick={() => setAssignments(g)} />
                    <IconButton icon={Pencil} label={t('common:edit')} onClick={() => setDrawer({ group: g })} />
                    <IconButton
                      icon={Trash2}
                      label={t('common:delete')}
                      variant="destructive"
                      disabled={g.channel_count > 0 || g.is_default}
                      onClick={() =>
                        confirm({
                          title: t('common:confirmDeleteTitle', { name: g.name }),
                          description: t('admin:proxyGroupDeleteConfirm'),
                          onConfirm: () => remove.mutate(g.code),
                        })
                      }
                    />
                  </div>
                </Td>
              </Tr>
            ))}
          </TBody>
        </Table>
      )}
      <Pagination {...pager} total={list.data?.total} />
      {drawer !== null && (
        <GroupDrawer
          group={drawer.group}
          onClose={() => setDrawer(null)}
          onDone={(r) => {
            report(r)
            invalidate()
          }}
        />
      )}
      {assignments !== null && <AssignmentsDrawer group={assignments} onClose={() => setAssignments(null)} />}
    </>
  )
}
