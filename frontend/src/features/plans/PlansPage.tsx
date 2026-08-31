import { Plus, Trash2 } from 'lucide-react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { EmptyState, ErrorState } from '@/components/ui/state'
import { IconButton } from '@/components/ui/icon-button'
import { PageHeader } from '@/components/ui/page'
import { PlanDrawer } from '@/features/plans/PlanDrawer'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatMoney } from '@/lib/money'
import { useConfirm } from '@/components/ui/confirm'

interface PlanRow {
  id: number
  plan_code: string
  display_name: string
  grant_micro: number
  group_code: string | null
  balance_valid_days: number | null
  status: number
  code_count: number
}


/// 套餐页。
///
/// 套餐是可复用的模板（发多少额度、进哪个分组、余额多久过期），兑换码只是它的
/// 分发载体，故与兑换码分开：改套餐会影响后续所有引用它的码。
export function PlansPage() {
  const { t, i18n } = useTranslation()
  const queryClient = useQueryClient()
  const [msg, setMsg] = useState<string | null>(null)
  const [drawer, setDrawer] = useState(false)
  const { confirm, dialog } = useConfirm()

  const plans = useQuery({
    queryKey: ['admin', 'plans'],
    queryFn: () => apiFetch<{ data: PlanRow[] }>('/admin/plans'),
  })
  const invalidate = () => void queryClient.invalidateQueries({ queryKey: ['admin', 'plans'] })

  const remove = useMutation({
    mutationFn: (code: string) =>
      apiFetch(`/admin/plans/${encodeURIComponent(code)}`, { method: 'DELETE' }),
    onSuccess: () => {
      setMsg(t('common:success'))
      invalidate()
    },
    onError: (err) => setMsg(describeError(err)),
  })

  const rows = plans.data?.data ?? []

  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        title={t('admin:planListTitle')}
        description={t('admin:planHint')}
        action={
          <Button onClick={() => setDrawer(true)}>
            <Plus className="h-4 w-4" />
            {t('admin:planUpsert')}
          </Button>
        }
      />

      {msg !== null && <p className="text-xs text-muted-foreground">{msg}</p>}
      {dialog}

      {plans.isError ? (
        <ErrorState message={describeError(plans.error)} />
      ) : rows.length === 0 ? (
        <EmptyState hint={t('admin:plansEmptyHint')} />
      ) : (
        <Table>
          <THead>
            <Tr>
              <Th>{t('admin:planCode')}</Th>
              <Th>{t('admin:planName')}</Th>
              <Th>{t('admin:planGrant')}</Th>
              <Th>{t('portal:group')}</Th>
              <Th>{t('admin:planValidDays')}</Th>
              <Th>{t('admin:planCodeCount')}</Th>
              <Th>{t('common:actions')}</Th>
            </Tr>
          </THead>
          <TBody>
            {rows.map((p) => (
              <Tr key={p.id}>
                <Td className="font-mono text-xs">{p.plan_code}</Td>
                <Td>{p.display_name}</Td>
                <Td>{formatMoney(p.grant_micro, i18n.language)}</Td>
                <Td>{p.group_code ?? '—'}</Td>
                <Td>{p.balance_valid_days ?? '—'}</Td>
                <Td>{p.code_count}</Td>
                <Td>
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
                </Td>
              </Tr>
            ))}
          </TBody>
        </Table>
      )}

      {drawer && (
        <PlanDrawer
          onClose={() => setDrawer(false)}
          onDone={() => {
            setMsg(t('common:success'))
            invalidate()
          }}
        />
      )}
    </div>
  )
}
