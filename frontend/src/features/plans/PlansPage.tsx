import { Package, Pencil, Plus, Trash2 } from 'lucide-react'
import { keepPreviousData, useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { TableSkeleton } from '@/components/ui/skeleton'
import { EmptyState, ErrorState } from '@/components/ui/state'
import { toast } from '@/components/ui/toast'
import { IconButton } from '@/components/ui/icon-button'
import { PageHeader } from '@/components/ui/page'
import { Pagination } from '@/components/ui/pagination'
import { PlanDrawer } from '@/features/plans/PlanDrawer'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { usePagination } from '@/hooks/use-pagination'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatMoney } from '@/lib/money'
import { qk } from '@/lib/query-keys'
import { useConfirm } from '@/components/ui/confirm'

interface PlanRow {
  id: number
  plan_code: string
  display_name: string
  kind: 0 | 1
  grant_micro: number
  group_code: string | null
  balance_valid_days: number | null
  price_micro: number
  period: 1 | 2 | 3 | null
  duration_days: number | null
  sort_order: number
  description: string | null
  status: number
  code_count: number
  active_subscribers: number
}

/// 套餐页。
///
/// 套餐是可复用的模板（发多少额度、进哪个分组、余额多久过期），兑换码只是它的
/// 分发载体，故与兑换码分开：改套餐会影响后续所有引用它的码。
/// 订阅形态（IMPLEMENTATION §11.28）同表同抽屉：额度列对订阅显示"每窗额度 / 周期"，
/// 有效期列显示订阅时长，多一列当前订阅人数。
export function PlansPage() {
  const { t, i18n } = useTranslation()
  const queryClient = useQueryClient()
  const [drawer, setDrawer] = useState<'create' | PlanRow | null>(null)
  const { confirm, dialog } = useConfirm()
  const pager = usePagination()

  const plans = useQuery({
    queryKey: [...qk.adminPlans, pager.offset, pager.limit],
    queryFn: () =>
      apiFetch<{ data: PlanRow[]; total: number }>(
        `/admin/plans?limit=${pager.limit}&offset=${pager.offset}`,
      ),
    placeholderData: keepPreviousData,
  })
  const invalidate = () => void queryClient.invalidateQueries({ queryKey: qk.adminPlans })

  const remove = useMutation({
    mutationFn: (code: string) =>
      apiFetch(`/admin/plans/${encodeURIComponent(code)}`, { method: 'DELETE' }),
    onSuccess: () => {
      toast.success(t('common:success'))
      invalidate()
    },
    onError: (err) => toast.error(describeError(err)),
  })

  const rows = plans.data?.data ?? []

  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        icon={Package}
        title={t('admin:planListTitle')}
        description={t('admin:planHint')}
        action={
          <Button onClick={() => setDrawer('create')}>
            <Plus className="h-4 w-4" />
            {t('admin:planCreate')}
          </Button>
        }
      />
      {dialog}

      {plans.isError ? (
        <ErrorState message={describeError(plans.error)} onRetry={() => void plans.refetch()} />
      ) : plans.isPending ? (
        <TableSkeleton rows={6} cols={9} />
      ) : rows.length === 0 ? (
        <EmptyState
          hint={t('admin:plansEmptyHint')}
          action={
            <Button onClick={() => setDrawer('create')}>
              <Plus className="h-4 w-4" />
              {t('admin:planCreate')}
            </Button>
          }
        />
      ) : (
        <Table stickyHeader>
          <THead>
            <Tr>
              <Th>{t('admin:planCode')}</Th>
              <Th>{t('admin:planName')}</Th>
              <Th>{t('admin:planKind')}</Th>
              <Th numeric>{t('admin:planGrant')}</Th>
              <Th numeric>{t('admin:planPrice')}</Th>
              <Th>{t('portal:group')}</Th>
              <Th numeric>{t('admin:planValidDays')}</Th>
              <Th numeric>{t('admin:planCodeCount')}</Th>
              <Th numeric>{t('admin:planSubscribers')}</Th>
              <Th>{t('common:actions')}</Th>
            </Tr>
          </THead>
          <TBody>
            {rows.map((p) => (
              <Tr key={p.id}>
                <Td className="font-mono text-xs">{p.plan_code}</Td>
                <Td>{p.display_name}</Td>
                <Td>
                  <Badge variant={p.kind === 1 ? 'default' : 'muted'}>
                    {t(`admin:planKind_${p.kind}`)}
                  </Badge>
                </Td>
                <Td numeric>
                  {formatMoney(p.grant_micro, i18n.language)}
                  {p.kind === 1 && p.period !== null && (
                    <span className="text-xs text-muted-foreground">
                      {' / '}
                      {t(`admin:planPeriod_${p.period}`)}
                    </span>
                  )}
                </Td>
                <Td numeric>
                  {p.kind === 1
                    ? p.price_micro > 0
                      ? formatMoney(p.price_micro, i18n.language)
                      : t('admin:planPriceNotForSale')
                    : '—'}
                </Td>
                <Td>{p.group_code ?? '—'}</Td>
                <Td numeric>{p.kind === 1 ? p.duration_days : p.balance_valid_days ?? '—'}</Td>
                <Td numeric>{p.code_count}</Td>
                <Td numeric>{p.kind === 1 ? p.active_subscribers : '—'}</Td>
                <Td>
                  <div className="flex items-center gap-0.5">
                    <IconButton
                      icon={Pencil}
                      label={t('common:edit')}
                      onClick={() => setDrawer(p)}
                    />
                    <IconButton
                      icon={Trash2}
                      label={t('common:delete')}
                      variant="destructive"
                      onClick={() =>
                        confirm({
                          title: t('common:confirmDeleteTitle', { name: p.plan_code }),
                          description: t('common:confirmPlanDelete'),
                          onConfirm: () => remove.mutate(p.plan_code),
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

      <Pagination {...pager} total={plans.data?.total} />

      {drawer !== null && (
        <PlanDrawer
          initial={drawer === 'create' ? undefined : drawer}
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
