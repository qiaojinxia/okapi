import { Ban, Plus } from 'lucide-react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { EmptyState, ErrorState } from '@/components/ui/state'
import { GenerateDrawer } from '@/features/codes/GenerateDrawer'
import { IconButton } from '@/components/ui/icon-button'
import { Label } from '@/components/ui/input'
import { PageHeader, Toolbar } from '@/components/ui/page'
import { Select } from '@/components/ui/select'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatMoney } from '@/lib/money'
import { useConfirm } from '@/components/ui/confirm'

interface CodeRow {
  id: number
  batch_id: string
  amount_micro: number
  status: number
  plan_code: string | null
  bind_user_id: number | null
  redeemed_by: number | null
  redeemed_at: string | null
  created_at: string
}



const CODE_STATUS = { unused: 1, used: 2, disabled: 3 } as const



/// 兑换码页。
///
/// 列表不含码明文（后端只存 SHA-256，生成时一次性返回），故"生成"是一次性动作，
/// 放在抽屉里并把结果留在抽屉内让用户复制，而不是塞在列表页顶部。
export function RedemptionsPage() {
  const { t, i18n } = useTranslation()
  const queryClient = useQueryClient()
  const [status, setStatus] = useState('')
  const [msg, setMsg] = useState<string | null>(null)
  const [drawer, setDrawer] = useState(false)
  const { confirm, dialog } = useConfirm()

  const codes = useQuery({
    queryKey: ['admin', 'redemptions', status],
    queryFn: () => {
      const params = new URLSearchParams({ limit: '100' })
      if (status !== '') params.set('status', status)
      return apiFetch<{ total: number; data: CodeRow[] }>(`/admin/redemptions?${params}`)
    },
  })
  const invalidate = () =>
    void queryClient.invalidateQueries({ queryKey: ['admin', 'redemptions'] })

  const disableBatch = useMutation({
    mutationFn: (batch: string) =>
      apiFetch<{ affected: number }>(`/admin/redemptions/${batch}`, { method: 'DELETE' }),
    onSuccess: (r) => {
      setMsg(t('admin:batchDisabled', { n: r.affected }))
      invalidate()
    },
    onError: (err) => setMsg(describeError(err)),
  })

  const statusLabel = (s: number) => {
    if (s === CODE_STATUS.used) return t('admin:codeUsed')
    if (s === CODE_STATUS.disabled) return t('common:disabled')
    return t('admin:codeUnused')
  }

  const rows = codes.data?.data ?? []

  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        title={t('admin:codeListTitle')}
        description={t('admin:codesDesc')}
        action={
          <Button onClick={() => setDrawer(true)}>
            <Plus className="h-4 w-4" />
            {t('admin:redeemGenerate')}
          </Button>
        }
      />

      <Toolbar
        filters={
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="cstatus">{t('common:status')}</Label>
            <Select
              id="cstatus"
              className="w-36"
              value={status}
              onChange={setStatus}
              placeholder={t('admin:codeAll')}
              options={[
                { value: '1', label: t('admin:codeUnused') },
                { value: '2', label: t('admin:codeUsed') },
                { value: '3', label: t('common:disabled') },
              ]}
            />
          </div>
        }
        selection={
          <span className="text-xs text-muted-foreground">
            {t('admin:keyTotal', { n: codes.data?.total ?? 0 })}
          </span>
        }
      />

      {msg !== null && <p className="text-xs text-muted-foreground">{msg}</p>}
      {dialog}

      {codes.isError ? (
        <ErrorState message={describeError(codes.error)} />
      ) : rows.length === 0 ? (
        <EmptyState hint={t('admin:codesEmptyHint')} />
      ) : (
        <Table>
          <THead>
            <Tr>
              <Th>ID</Th>
              <Th>{t('admin:codeBatch')}</Th>
              <Th>{t('common:amount')}</Th>
              <Th>{t('common:status')}</Th>
              <Th>{t('admin:codePlan')}</Th>
              <Th>{t('admin:codeRedeemedBy')}</Th>
              <Th>{t('common:actions')}</Th>
            </Tr>
          </THead>
          <TBody>
            {rows.map((c) => (
              <Tr key={c.id}>
                <Td>{c.id}</Td>
                <Td className="font-mono text-xs">{c.batch_id.slice(0, 8)}…</Td>
                <Td>{formatMoney(c.amount_micro, i18n.language)}</Td>
                <Td>
                  <Badge variant={c.status === CODE_STATUS.unused ? 'success' : 'muted'}>
                    {statusLabel(c.status)}
                  </Badge>
                </Td>
                <Td>{c.plan_code ?? '—'}</Td>
                <Td>{c.redeemed_by ?? '—'}</Td>
                <Td>
                  <IconButton
                    icon={Ban}
                    label={t('admin:disableBatch')}
                    variant="destructive"
                    disabled={c.status !== CODE_STATUS.unused}
                    onClick={() =>
                      confirm({
                        title: t('admin:disableBatch'),
                        description: t('admin:disableBatchHint'),
                        confirmLabel: t('admin:disableBatch'),
                        onConfirm: () => disableBatch.mutate(c.batch_id),
                      })
                    }
                  />
                </Td>
              </Tr>
            ))}
          </TBody>
        </Table>
      )}

      {drawer && <GenerateDrawer onClose={() => setDrawer(false)} onDone={invalidate} />}
    </div>
  )
}
