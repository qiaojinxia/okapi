import { Pencil, Plus, Trash2 } from 'lucide-react'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { GroupListRow } from '@/features/groups/types'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { ListState } from '@/components/ui/list-state'
import { EmptyState } from '@/components/ui/state'
import { toast } from '@/components/ui/toast'
import { GroupDrawer } from '@/features/groups/GroupDrawer'
import { ActionGroup, IconButton } from '@/components/ui/icon-button'
import { PageHeader } from '@/components/ui/page'
import { Pagination } from '@/components/ui/pagination'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { pagedPath, usePagedQuery } from '@/hooks/use-paged-query'
import type { Paged } from '@/hooks/use-paged-query'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatRatio } from '@/lib/money'
import { qk } from '@/lib/query-keys'
import { useConfirm } from '@/components/ui/confirm'

/// 价格分组页。
///
/// 分组倍率是"整层加价/打折"，与单个模型的倍率是两个不同决策，故独立成页。
/// 列表带占用计数：被用户或渠道引用时后端回 409，先把数字摆出来省掉无谓尝试。
export function GroupsPage() {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const [drawer, setDrawer] = useState<{ group?: GroupListRow } | null>(null)
  const { confirm, dialog } = useConfirm()
  const { query: groups, pager, rows } = usePagedQuery({
    queryKey: (page) => qk.adminGroupsPage(page),
    queryFn: (page) => apiFetch<Paged<GroupListRow>>(pagedPath('/admin/groups', page)),
  })
  const invalidate = () => void queryClient.invalidateQueries({ queryKey: qk.adminGroups })

  const remove = useMutation({
    mutationFn: (code: string) =>
      apiFetch(`/admin/groups/${encodeURIComponent(code)}`, { method: 'DELETE' }),
    onSuccess: () => {
      toast.warning(t('admin:requiresPublish'))
      invalidate()
    },
    onError: (err) => toast.error(describeError(err)),
  })

  return (
    <div className="list-page">
      <PageHeader
        className="shrink-0"
        title={t('admin:groupsTitle')}
        description={t('admin:groupsDesc')}
        meta={groups.data?.total === undefined ? undefined : t('common:totalCount', { n: groups.data.total })}
        action={
          <Button onClick={() => setDrawer({})}>
            <Plus className="h-4 w-4" />
            {t('admin:groupCreate')}
          </Button>
        }
      />
      {dialog}

      <ListState
        query={groups}
        skeleton={{ rows: 6, cols: 8 }}
        empty={
          <EmptyState
            hint={t('admin:groupsDesc')}
            action={
              <Button onClick={() => setDrawer({})}>
                <Plus className="h-4 w-4" />
                {t('admin:groupCreate')}
              </Button>
            }
          />
        }
      >
        <Table
          stickyHeader
          className="min-w-[66rem] table-fixed"
          aria-label={t('admin:groupsTitle')}
          scrollResetKey={`${pager.offset}:${pager.limit}`}
          aria-busy={groups.isFetching}
        >
          <colgroup>
            <col className="w-64" />
            <col className="w-20" />
            <col />
            <col className="w-20" />
            <col className="w-36" />
            <col className="w-20" />
            <col className="w-44" />
            <col className="w-28 md:w-24" />
          </colgroup>
          <THead>
            <Tr>
              <Th>{t('admin:groupCode')}</Th>
              <Th numeric>{t('admin:groupRatio')}</Th>
              <Th>{t('admin:groupDesc')}</Th>
              <Th numeric>{t('admin:groupUsers')}</Th>
              <Th>{t('admin:groupPool')}</Th>
              <Th numeric>{t('admin:groupChannels')}</Th>
              <Th numeric>{t('admin:groupRateLimit')}</Th>
              <Th className="text-right">{t('common:actions')}</Th>
            </Tr>
          </THead>
          <TBody>
            {rows.map((g) => {
              const rateLimit = g.rpm_limit === null && g.rph_limit === null
                ? '—'
                : t('admin:groupRateCell', { rpm: g.rpm_limit ?? '∞', rph: g.rph_limit ?? '∞' })
              return (
                <Tr key={g.group_code}>
                  <Td>
                    <div className="flex min-w-0 flex-col items-start gap-0.5">
                      <button
                        type="button"
                        onClick={() => setDrawer({ group: g })}
                        className="max-w-full cursor-pointer truncate rounded-sm text-left font-mono font-medium outline-none transition-colors hover:text-primary hover:underline hover:underline-offset-4 focus-visible:ring-2 focus-visible:ring-primary/40"
                        title={g.group_code}
                      >
                        {g.group_code}
                      </button>
                      {(g.is_default || g.self_select || g.published === false) && (
                        <div className="flex flex-wrap items-center gap-1">
                          {g.published === false && (
                            <Badge variant="warning" className="min-h-5 py-0" title={t('admin:groupUnpublishedHint')}>
                              {t('admin:groupUnpublished')}
                            </Badge>
                          )}
                          {g.is_default && (
                            <Badge variant="muted" className="min-h-5 py-0">
                              {t('admin:groupDefault')}
                            </Badge>
                          )}
                          {g.self_select && (
                            <Badge variant="success" className="min-h-5 py-0">
                              {t('admin:groupSelfSelectBadge')}
                            </Badge>
                          )}
                        </div>
                      )}
                    </div>
                  </Td>
                  <Td numeric className="font-medium">×{formatRatio(g.group_ratio ?? '1')}</Td>
                  <Td className="truncate text-xs text-muted-foreground" title={g.description ?? undefined}>
                    {g.description ?? '—'}
                  </Td>
                  <Td numeric>{g.user_count}</Td>
                  <Td title={g.pool_code}>
                    <span className="inline-flex max-w-full items-center truncate rounded bg-muted/85 px-1.5 py-0.5 font-mono text-[11px] font-medium text-muted-foreground ring-1 ring-inset ring-border/60 dark:bg-white/[0.06]">
                      {g.pool_code}
                    </span>
                  </Td>
                  <Td numeric>
                    {/* 池里零渠道 = 这个分组的用户什么都打不到，与"未定价"同类的配了一半 */}
                    {g.channel_count === 0 ? (
                      <Badge variant="destructive">{t('admin:poolNoChannel')}</Badge>
                    ) : (
                      g.channel_count
                    )}
                  </Td>
                  <Td numeric className="truncate text-xs text-muted-foreground" title={rateLimit}>
                    {rateLimit}
                  </Td>
                  <Td>
                    <div className="flex items-center justify-end gap-0.5">
                      <ActionGroup label={t('common:actions')}>
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
                      </ActionGroup>
                    </div>
                  </Td>
                </Tr>
              )
            })}
          </TBody>
        </Table>
      </ListState>

      <Pagination {...pager} total={groups.data?.total} className="shrink-0" />

      {drawer !== null && (
        <GroupDrawer
          group={drawer.group}
          onClose={() => setDrawer(null)}
          onDone={() => {
            toast.warning(t('admin:requiresPublish'))
            invalidate()
          }}
        />
      )}
    </div>
  )
}
