import { useMutation, useQueryClient } from '@tanstack/react-query'
import { Pencil, Play, Plus, Trash2 } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { useConfirm } from '@/components/ui/confirm'
import { ActionGroup, IconButton } from '@/components/ui/icon-button'
import { ListState } from '@/components/ui/list-state'
import { PageHeader } from '@/components/ui/page'
import { Pagination } from '@/components/ui/pagination'
import { EmptyState } from '@/components/ui/state'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { toast } from '@/components/ui/toast'
import { PoolDrawer } from '@/features/pools/PoolDrawer'
import type { PoolDrawerTab } from '@/features/pools/PoolDrawer'
import { STRATEGY_LABEL, strategyOf } from '@/features/pools/types'
import type { PoolRow } from '@/features/pools/types'
import { pagedPath, usePagedQuery } from '@/hooks/use-paged-query'
import type { Paged } from '@/hooks/use-paged-query'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { qk } from '@/lib/query-keys'

/// 调度池与策略页（Routing Pools & Policies）。
///
/// 调度池承载渠道成员集合 + 声明式调度策略（多跳降级、灰度切流、长短上下文分流、护栏与 Dry-Run 模拟），
/// 与价格分组的"付多少钱"正交分离。
export function PoolsPage() {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const [drawer, setDrawer] = useState<{ pool?: PoolRow; tab?: PoolDrawerTab } | null>(null)
  const { confirm, dialog } = useConfirm()
  const { query: pools, pager, rows } = usePagedQuery({
    queryKey: (page) => qk.adminPoolsPage(page),
    queryFn: (page) => apiFetch<Paged<PoolRow>>(pagedPath('/admin/pools', page)),
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

  return (
    <div className="list-page">
      <PageHeader
        title={t('admin:poolsPageTitle')}
        description={t('admin:poolsDesc')}
        meta={t('common:totalCount', { n: pools.data?.total ?? 0 })}
        action={
          <Button onClick={() => setDrawer({})}>
            <Plus className="h-4 w-4" />
            {t('admin:poolCreate')}
          </Button>
        }
      />
      {dialog}

      <ListState
        query={pools}
        skeleton={{ rows: 6, cols: 7 }}
        empty={
          <EmptyState
            hint={t('admin:poolsEmptyHint')}
            action={
              <Button onClick={() => setDrawer({})}>
                <Plus className="h-4 w-4" />
                {t('admin:poolCreate')}
              </Button>
            }
          />
        }
      >
        <Table stickyHeader>
          <THead>
            <Tr>
              <Th>{t('admin:poolCode')}</Th>
              <Th>{t('admin:poolStrategy')}</Th>
              <Th>{t('admin:poolFallback')}</Th>
              <Th>{t('common:description')}</Th>
              <Th numeric>{t('admin:poolChannels')}</Th>
              <Th>{t('admin:poolRefs')}</Th>
              <Th className="text-right">{t('common:actions')}</Th>
            </Tr>
          </THead>
          <TBody>
            {rows.map((p) => {
              const refs = p.group_count + p.key_count + p.fallback_ref_count
              const fallbacks =
                p.policy?.fallback_pools && p.policy.fallback_pools.length > 0
                  ? p.policy.fallback_pools
                  : p.fallback_pool_code
                    ? [p.fallback_pool_code]
                    : []
              const rulesCount = p.policy?.rules?.length ?? 0
              const canaryPct = p.policy?.canary_percent ?? 0
              return (
                <Tr key={p.pool_code}>
                  <Td>
                    <span className="inline-flex items-center gap-1.5">
                      <button
                        type="button"
                        onClick={() => setDrawer({ pool: p })}
                        className="cursor-pointer rounded-sm text-left font-mono text-xs font-medium outline-none transition-colors hover:text-primary hover:underline hover:underline-offset-4 focus-visible:ring-2 focus-visible:ring-primary/40"
                      >
                        {p.pool_code}
                      </button>
                      {p.builtin && <Badge variant="muted">{t('admin:poolBuiltin')}</Badge>}
                      {(p.policy_version ?? 1) > 1 && (
                        <Badge variant="muted" className="font-mono text-[10px]">
                          {t('admin:poolPolicyVersion', { v: p.policy_version })}
                        </Badge>
                      )}
                    </span>
                  </Td>
                  <Td>
                    <div className="flex flex-wrap items-center gap-1">
                      <Badge
                        variant={
                          strategyOf(p.routing_strategy) === 'priority_weighted'
                            ? 'muted'
                            : 'default'
                        }
                      >
                        {t(STRATEGY_LABEL[strategyOf(p.routing_strategy)])}
                      </Badge>
                      {canaryPct > 0 && (
                        <Badge variant="default" className="text-[10px]">
                          {t('admin:poolPolicyCanaryBadge', { pct: canaryPct })}
                        </Badge>
                      )}
                      {rulesCount > 0 && (
                        <Badge variant="muted" className="text-[10px]">
                          {t('admin:poolPolicyRulesCount', { n: rulesCount })}
                        </Badge>
                      )}
                    </div>
                  </Td>
                  <Td>
                    {fallbacks.length > 0 ? (
                      <span className="inline-flex max-w-full items-center truncate rounded bg-muted/85 px-1.5 py-0.5 font-mono text-[11px] font-medium text-muted-foreground ring-1 ring-inset ring-border/60 dark:bg-white/[0.06]">
                        {fallbacks.join(' → ')}
                      </span>
                    ) : (
                      <span className="text-xs text-muted-foreground">—</span>
                    )}
                  </Td>
                  <Td className="max-w-64 truncate text-xs text-muted-foreground">
                    {p.description ?? '—'}
                  </Td>
                  <Td numeric>
                    {p.channel_count === 0 ? (
                      <Badge variant="destructive">{t('admin:poolNoChannel')}</Badge>
                    ) : (
                      p.channel_count
                    )}
                  </Td>
                  <Td className="whitespace-nowrap text-xs text-muted-foreground">
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
                    <div className="flex items-center justify-end">
                      <ActionGroup label={t('common:actions')}>
                        <IconButton
                          icon={Play}
                          label={t('admin:poolSimAction')}
                          onClick={() => setDrawer({ pool: p, tab: 'simulate' })}
                        />
                        <IconButton
                          icon={Pencil}
                          label={t('common:edit')}
                          onClick={() => setDrawer({ pool: p, tab: 'basic' })}
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
                      </ActionGroup>
                    </div>
                  </Td>
                </Tr>
              )
            })}
          </TBody>
        </Table>
      </ListState>

      <Pagination {...pager} total={pools.data?.total} />

      {drawer !== null && (
        <PoolDrawer
          pool={drawer.pool}
          initialTab={drawer.tab}
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
