import { useMutation, useQueryClient } from '@tanstack/react-query'
import { ListTree, Pencil, Plus, Trash2 } from 'lucide-react'
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
import { AssignmentsDrawer } from './AssignmentsDrawer'
import { GroupDrawer } from './GroupDrawer'
import { useReportToast } from './report'
import type { ProxyGroupRow } from './types'
import { GROUP_MODE_LABEL } from './types'
import { ListToolbar } from './ListToolbar'

export function GroupList({ tabs }: { tabs: React.ReactNode }) {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const { confirm, dialog } = useConfirm()
  const [drawer, setDrawer] = useState<{ group?: ProxyGroupRow } | null>(null)
  const [assignments, setAssignments] = useState<ProxyGroupRow | null>(null)
  const report = useReportToast()
  const { query: list, pager, rows } = usePagedQuery({
    queryKey: (page) => qk.adminProxyGroupsPage(page),
    queryFn: (page) => apiFetch<Paged<ProxyGroupRow>>(pagedPath('/admin/proxy-groups', page)),
  })
  const invalidate = () => void invalidateEgress(queryClient)
  const remove = useMutation({
    mutationFn: (code: string) =>
      apiFetch(`/admin/proxy-groups/${encodeURIComponent(code)}`, { method: 'DELETE' }),
    onSuccess: () => {
      toast.success(t('common:success'))
      invalidate()
    },
    onError: (err) => toast.error(describeError(err)),
  })
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
      <ListState query={list} skeleton={{ rows: 4, cols: 5 }} empty={<EmptyState hint={t('admin:proxyGroupsEmptyHint')} action={create} />}>
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
      </ListState>
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
