import { Pencil, Plus, Trash2 } from 'lucide-react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { GroupListRow } from '@/features/groups/types'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { EmptyState, ErrorState } from '@/components/ui/state'
import { GroupDrawer } from '@/features/groups/GroupDrawer'
import { IconButton } from '@/components/ui/icon-button'
import { PageHeader } from '@/components/ui/page'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { qk } from '@/lib/query-keys'
import { useConfirm } from '@/components/ui/confirm'

/// 价格分组页。
///
/// 分组倍率是"整层加价/打折"，与单个模型的倍率是两个不同决策，故独立成页。
/// 列表带占用计数：被用户或渠道引用时后端回 409，先把数字摆出来省掉无谓尝试。
export function GroupsPage() {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const [msg, setMsg] = useState<string | null>(null)
  const [drawer, setDrawer] = useState<{ group?: GroupListRow } | null>(null)
  const { confirm, dialog } = useConfirm()

  const groups = useQuery({
    queryKey: qk.adminGroups,
    queryFn: () => apiFetch<{ data: GroupListRow[] }>('/admin/groups'),
  })
  const invalidate = () => void queryClient.invalidateQueries({ queryKey: qk.adminGroups })

  const remove = useMutation({
    mutationFn: (code: string) =>
      apiFetch(`/admin/groups/${encodeURIComponent(code)}`, { method: 'DELETE' }),
    onSuccess: () => {
      setMsg(t('admin:requiresPublish'))
      invalidate()
    },
    onError: (err) => setMsg(describeError(err)),
  })

  const rows = groups.data?.data ?? []

  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        title={t('admin:groupsTitle')}
        description={t('admin:groupsDesc')}
        action={
          <Button onClick={() => setDrawer({})}>
            <Plus className="h-4 w-4" />
            {t('admin:groupCreate')}
          </Button>
        }
      />

      {msg !== null && <p className="text-xs text-muted-foreground">{msg}</p>}
      {dialog}

      {groups.isError ? (
        <ErrorState message={describeError(groups.error)} />
      ) : rows.length === 0 ? (
        <EmptyState />
      ) : (
        <Table>
          <THead>
            <Tr>
              <Th>{t('admin:groupCode')}</Th>
              <Th>{t('admin:groupRatio')}</Th>
              <Th>{t('admin:groupDesc')}</Th>
              <Th>{t('admin:groupUsers')}</Th>
              <Th>{t('admin:groupChannels')}</Th>
              <Th>{t('common:actions')}</Th>
            </Tr>
          </THead>
          <TBody>
            {rows.map((g) => (
              <Tr key={g.group_code}>
                <Td className="font-mono text-xs">
                  {g.group_code}
                  {g.is_default && (
                    <Badge variant="muted" className="ml-2">
                      {t('admin:groupDefault')}
                    </Badge>
                  )}
                </Td>
                <Td>×{g.group_ratio ?? '1'}</Td>
                <Td className="max-w-64 truncate text-xs text-muted-foreground">
                  {g.description ?? '—'}
                </Td>
                <Td>{g.user_count}</Td>
                <Td>{g.channel_count}</Td>
                <Td>
                  <div className="flex items-center gap-0.5">
                    <IconButton
                      icon={Pencil}
                      label={t('common:edit')}
                      onClick={() => setDrawer({ group: g })}
                    />
                    <IconButton
                      icon={Trash2}
                      label={t('common:delete')}
                      variant="destructive"
                      disabled={g.is_default}
                      onClick={() =>
                        confirm({
                          title: t('common:confirmDeleteTitle', { name: g.group_code }),
                          description: t('common:confirmGroupDelete'),
                          onConfirm: () => remove.mutate(g.group_code),
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

      {drawer !== null && (
        <GroupDrawer
          group={drawer.group}
          onClose={() => setDrawer(null)}
          onDone={() => {
            setMsg(t('admin:requiresPublish'))
            invalidate()
          }}
        />
      )}
    </div>
  )
}
