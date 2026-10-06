import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Drawer } from '@/components/ui/drawer'
import { Select } from '@/components/ui/select'
import { TableSkeleton } from '@/components/ui/skeleton'
import { EmptyState, ErrorState } from '@/components/ui/state'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { toast } from '@/components/ui/toast'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { qk } from '@/lib/query-keys'
import type { AssignmentRow, ProxyGroupRow } from './types'

/// 固定分配明细：「哪个账号（key）在哪个出口上」一览，可手动改分到组内另一个代理
/// （目标满了后端回 409 proxy_full）。经全局默认继承该组的渠道也列在这里。
export function AssignmentsDrawer({ group, onClose }: { group: ProxyGroupRow; onClose: () => void }) {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const rows = useQuery({
    queryKey: qk.proxyGroupAssignments(group.code),
    queryFn: () =>
      apiFetch<{ data: AssignmentRow[] }>(
        `/admin/proxy-groups/${encodeURIComponent(group.code)}/assignments`,
      ),
  })
  const move = useMutation({
    mutationFn: ({ key, proxy }: { key: number; proxy: number }) =>
      apiFetch(`/admin/proxy-groups/${encodeURIComponent(group.code)}/assignments`, {
        method: 'POST',
        body: { key_id: key, proxy_id: proxy },
      }),
    onSuccess: () => {
      toast.success(t('common:success'))
      void queryClient.invalidateQueries({ queryKey: qk.adminProxyGroups })
      void queryClient.invalidateQueries({ queryKey: qk.adminProxies })
      void queryClient.invalidateQueries({ queryKey: qk.adminChannels })
    },
    onError: (err) => toast.error(describeError(err)),
  })
  const data = rows.data?.data ?? []
  const memberName = (id: number | null) =>
    id === null ? null : (group.members.find((m) => m.proxy_id === id)?.name ?? `#${id}`)

  return (
    <Drawer
      open
      onClose={onClose}
      title={t('admin:proxyAssignmentsTitle', { name: group.name })}
      description={t('admin:proxyAssignmentsDesc')}
      footer={
        <Button variant="ghost" onClick={onClose}>
          {t('common:close')}
        </Button>
      }
    >
      {rows.isError ? (
        <ErrorState message={describeError(rows.error)} onRetry={() => void rows.refetch()} />
      ) : rows.isPending ? (
        <TableSkeleton rows={4} cols={3} />
      ) : data.length === 0 ? (
        <EmptyState hint={t('admin:proxyAssignmentsEmpty')} />
      ) : (
        <Table>
          <THead>
            <Tr>
              <Th>{t('admin:channelName')}</Th>
              <Th>{t('admin:proxyAssignedTo')}</Th>
              {group.mode === 'pinned' && <Th>{t('admin:proxyAssignmentMove')}</Th>}
            </Tr>
          </THead>
          <TBody>
            {data.map((row) => (
              <Tr key={row.key_id}>
                <Td>
                  <span className="flex flex-col">
                    <span className="truncate font-medium">{row.channel_name}</span>
                    <span className="text-xs text-muted-foreground tabular-nums">
                      #{row.channel_id} · key #{row.key_id}
                    </span>
                  </span>
                </Td>
                <Td>
                  {group.mode === 'rotate' ? (
                    <Badge variant="muted">{t('admin:proxyGroupRotate')}</Badge>
                  ) : row.proxy_id === null ? (
                    <Badge variant="warning">{t('admin:proxyUnassigned')}</Badge>
                  ) : (
                    <span className="text-sm">{memberName(row.proxy_id)}</span>
                  )}
                </Td>
                {group.mode === 'pinned' && (
                  <Td>
                    <Select
                      aria-label={t('admin:proxyAssignmentMove')}
                      className="w-48"
                      value=""
                      placeholder={t('admin:proxyAssignmentMovePick')}
                      disabled={move.isPending}
                      onChange={(id) => {
                        if (id !== '') move.mutate({ key: row.key_id, proxy: Number(id) })
                      }}
                      options={group.members
                        .filter((m) => m.proxy_id !== row.proxy_id)
                        .map((m) => ({
                          value: String(m.proxy_id),
                          label: `${m.name}${m.max_keys !== null ? ` (${m.assigned_keys}/${m.max_keys})` : ''}`,
                        }))}
                    />
                  </Td>
                )}
              </Tr>
            ))}
          </TBody>
        </Table>
      )}
    </Drawer>
  )
}
