import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Boxes, Pencil, Plus, Trash2 } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { useConfirm } from '@/components/ui/confirm'
import { IconButton } from '@/components/ui/icon-button'
import { PageHeader } from '@/components/ui/page'
import { TableSkeleton } from '@/components/ui/skeleton'
import { EmptyState, ErrorState } from '@/components/ui/state'
import { toast } from '@/components/ui/toast'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { PoolDrawer } from '@/features/pools/PoolDrawer'
import type { PoolRow } from '@/features/pools/types'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { qk } from '@/lib/query-keys'

/// 渠道池页。
///
/// 池回答的是"打哪些上游、怎么在里面选"，与价格分组的"付多少钱"是两个决策。
/// 列表把引用数摆出来：被分组或令牌引用的池删不掉（后端 409），先看到数字
/// 就不必去试。
export function PoolsPage() {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const [drawer, setDrawer] = useState<{ pool?: PoolRow } | null>(null)
  const { confirm, dialog } = useConfirm()

  const pools = useQuery({
    queryKey: qk.adminPools,
    queryFn: () => apiFetch<{ data: PoolRow[] }>('/admin/pools'),
  })
  const invalidate = () => void queryClient.invalidateQueries({ queryKey: qk.adminPools })

  const remove = useMutation({
    mutationFn: (code: string) =>
      apiFetch(`/admin/pools/${encodeURIComponent(code)}`, { method: 'DELETE' }),
    onSuccess: () => {
      toast.success(t('common:success'))
      invalidate()
    },
    onError: (err) => toast.error(describeError(err)),
  })

  const rows = pools.data?.data ?? []

  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        icon={Boxes}
        title={t('admin:poolsTitle')}
        description={t('admin:poolsDesc')}
        meta={<Badge variant="muted">{t('admin:keyTotal', { n: rows.length })}</Badge>}
        action={
          <Button onClick={() => setDrawer({})}>
            <Plus className="h-4 w-4" />
            {t('admin:poolCreate')}
          </Button>
        }
      />
      {dialog}

      {pools.isError ? (
        <ErrorState message={describeError(pools.error)} onRetry={() => void pools.refetch()} />
      ) : pools.isPending ? (
        <TableSkeleton rows={6} cols={6} />
      ) : rows.length === 0 ? (
        <EmptyState
          hint={t('admin:poolsEmptyHint')}
          action={
            <Button onClick={() => setDrawer({})}>
              <Plus className="h-4 w-4" />
              {t('admin:poolCreate')}
            </Button>
          }
        />
      ) : (
        <Table>
          <THead>
            <Tr>
              <Th>{t('admin:poolCode')}</Th>
              <Th>{t('admin:poolStrategy')}</Th>
              <Th>{t('admin:poolFallback')}</Th>
              <Th>{t('common:description')}</Th>
              <Th>{t('admin:poolChannels')}</Th>
              <Th>{t('admin:poolRefs')}</Th>
              <Th>{t('common:actions')}</Th>
            </Tr>
          </THead>
          <TBody>
            {rows.map((p) => {
              const refs = p.group_count + p.key_count + p.fallback_ref_count
              return (
                <Tr key={p.pool_code}>
                  <Td>
                    <span className="inline-flex items-center gap-1.5">
                      <span className="font-mono text-xs">{p.pool_code}</span>
                      {p.builtin && <Badge variant="muted">{t('admin:poolBuiltin')}</Badge>}
                    </span>
                  </Td>
                  <Td>
                    <Badge variant={p.routing_strategy === 'least_latency' ? 'default' : 'muted'}>
                      {t(
                        p.routing_strategy === 'least_latency'
                          ? 'admin:strategyLeastLatency'
                          : 'admin:strategyPriorityWeighted',
                      )}
                    </Badge>
                  </Td>
                  <Td className="font-mono text-xs">
                    {p.fallback_pool_code ?? <span className="text-muted-foreground">—</span>}
                  </Td>
                  <Td className="max-w-64 truncate text-xs text-muted-foreground">
                    {p.description ?? '—'}
                  </Td>
                  <Td>
                    {p.channel_count === 0 ? (
                      <Badge variant="destructive">{t('admin:poolNoChannel')}</Badge>
                    ) : (
                      p.channel_count
                    )}
                  </Td>
                  <Td className="text-xs text-muted-foreground">
                    {refs === 0
                      ? '—'
                      : t('admin:poolRefsDetail', {
                          groups: p.group_count,
                          keys: p.key_count,
                        }) +
                        (p.fallback_ref_count > 0
                          ? ` · ${t('admin:poolRefsFallback', { n: p.fallback_ref_count })}`
                          : '')}
                  </Td>
                  <Td>
                    <div className="flex items-center gap-0.5">
                      {/* 抽屉早就支持 pool 回填（策略/说明），页面却一直没给编辑入口 */}
                      <IconButton
                        icon={Pencil}
                        label={t('common:edit')}
                        onClick={() => setDrawer({ pool: p })}
                      />
                      <IconButton
                        icon={Trash2}
                        label={t('common:delete')}
                        variant="destructive"
                        disabled={refs > 0 || p.builtin}
                        onClick={() =>
                          confirm({
                            title: t('common:confirmDeleteTitle', { name: p.pool_code }),
                            description: t('admin:confirmPoolDelete'),
                            onConfirm: () => remove.mutate(p.pool_code),
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

      {drawer !== null && (
        <PoolDrawer
          pool={drawer.pool}
          onClose={() => setDrawer(null)}
          onDone={() => {
            toast.success(t('common:success'))
            invalidate()
          }}
        />
      )}
    </div>
  )
}
