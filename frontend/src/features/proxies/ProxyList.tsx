import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Activity, Pencil, Plus, Trash2, Upload } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { useConfirm } from '@/components/ui/confirm'
import { IconButton } from '@/components/ui/icon-button'
import { ListState } from '@/components/ui/list-state'
import { Pagination } from '@/components/ui/pagination'
import { EmptyState } from '@/components/ui/state'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { toast } from '@/components/ui/toast'
import { pagedPath, usePagedQuery } from '@/hooks/use-paged-query'
import type { Paged } from '@/hooks/use-paged-query'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { invalidateEgress } from '@/lib/invalidate'
import { qk } from '@/lib/query-keys'
import { ImportDrawer } from './ImportDrawer'
import { ProxyDrawer } from './ProxyDrawer'
import { proxyTrafficOptions } from './options'
import { formatBytes } from '@/features/monitor/format'
import { useReportToast } from './report'
import type { ProbeResult, ProxyRow } from './types'
import { exitIpRecentlyChanged } from './types'
import { ListToolbar } from './ListToolbar'

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

/// 列表里的近 24 小时流量：合计加上下行。
function TrafficCell({ row }: { row: { up_bytes: number; down_bytes: number } | undefined }) {
  const { t, i18n } = useTranslation()
  if (!row) return <span className="text-muted-foreground">—</span>
  return (
    <span className="flex flex-col items-end tabular-nums">
      <span className="font-medium">{formatBytes(row.up_bytes + row.down_bytes, i18n.language)}</span>
      <span className="text-muted-foreground">
        {t('admin:proxyTrafficSplit', {
          down: formatBytes(row.down_bytes, i18n.language),
          up: formatBytes(row.up_bytes, i18n.language),
        })}
      </span>
    </span>
  )
}

export function ProxyList({ tabs }: { tabs: React.ReactNode }) {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const { confirm, dialog } = useConfirm()
  const [drawer, setDrawer] = useState<{ proxy?: ProxyRow } | null>(null)
  const [importing, setImporting] = useState(false)
  const report = useReportToast()
  const { query: list, pager, rows } = usePagedQuery({
    queryKey: (page) => qk.adminProxiesPage(page),
    queryFn: (page) => apiFetch<Paged<ProxyRow>>(pagedPath('/admin/proxies', page)),
  })
  // 近 24 小时流量是附加列：没有 ClickHouse（501）或查不到时只显示「—」，不挡列表
  const traffic = useQuery({ ...proxyTrafficOptions('24h'), retry: false })
  const trafficOf = (id: number) => traffic.data?.proxies.find((r) => r.proxy_id === id)
  const invalidate = () => void invalidateEgress(queryClient)
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
      <ListState query={list} skeleton={{ rows: 6, cols: 7 }} empty={<EmptyState hint={t('admin:proxiesEmptyHint')} action={create} />}>
        <Table stickyHeader>
          <THead>
            <Tr>
              <Th>{t('admin:proxyName')}</Th>
              <Th>{t('common:status')}</Th>
              <Th>{t('admin:proxyExit')}</Th>
              <Th>{t('admin:proxyUsage')}</Th>
              <Th numeric>{t('admin:proxyTraffic24h')}</Th>
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
                  <Td numeric className="text-xs">
                    <TrafficCell row={trafficOf(p.id)} />
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
      </ListState>
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
