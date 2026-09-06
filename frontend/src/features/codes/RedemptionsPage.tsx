import { Ban, Plus, Ticket } from 'lucide-react'
import { keepPreviousData, useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { getRouteApi } from '@tanstack/react-router'
import dayjs from 'dayjs'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { TableSkeleton } from '@/components/ui/skeleton'
import { EmptyState, ErrorState } from '@/components/ui/state'
import { toast } from '@/components/ui/toast'
import { GenerateDrawer } from '@/features/codes/GenerateDrawer'
import { CODE_STATUS, CODE_STATUS_FILTERS } from '@/features/codes/types'
import { IconButton } from '@/components/ui/icon-button'
import { Label } from '@/components/ui/input'
import { PageHeader, Toolbar } from '@/components/ui/page'
import { Pagination } from '@/components/ui/pagination'
import { Select } from '@/components/ui/select'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { usePagination } from '@/hooks/use-pagination'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatMoney } from '@/lib/money'
import { qk } from '@/lib/query-keys'
import { oneOf } from '@/lib/search-params'
import { useConfirm } from '@/components/ui/confirm'

const routeApi = getRouteApi('/admin/codes')

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

/// 兑换码页。
///
/// 列表不含码明文（后端只存 SHA-256，生成时一次性返回），故"生成"是一次性动作，
/// 放在抽屉里并把结果留在抽屉内让用户复制，而不是塞在列表页顶部。
export function RedemptionsPage() {
  const { t, i18n } = useTranslation()
  const queryClient = useQueryClient()
  // 状态筛选与页码都在地址里。此前写死 limit=100：一批码就能生成几百上千张，
  // 超出的那部分在页面上根本看不到
  const search = routeApi.useSearch()
  const navigate = routeApi.useNavigate()
  const status = search.status ?? ''
  const setStatus = (value: string) =>
    void navigate({ search: (prev) => ({ ...prev, status: oneOf(value, CODE_STATUS_FILTERS), page: undefined }) })
  const [drawer, setDrawer] = useState(false)
  const { confirm, dialog } = useConfirm()
  const pager = usePagination()

  const codes = useQuery({
    queryKey: [...qk.adminRedemptions(status), pager.offset, pager.limit],
    queryFn: () => {
      const params = new URLSearchParams({
        limit: String(pager.limit),
        offset: String(pager.offset),
      })
      if (status !== '') params.set('status', status)
      return apiFetch<{ total: number; data: CodeRow[] }>(`/admin/redemptions?${params}`)
    },
    // 翻页时保留上一页数据：表格不闪成骨架屏
    placeholderData: keepPreviousData,
  })
  const invalidate = () => void queryClient.invalidateQueries({ queryKey: qk.adminRedemptionsAll })

  const disableBatch = useMutation({
    mutationFn: (batch: string) =>
      apiFetch<{ affected: number }>(`/admin/redemptions/${batch}`, { method: 'DELETE' }),
    onSuccess: (r) => {
      toast.success(t('admin:batchDisabled', { n: r.affected }))
      invalidate()
    },
    onError: (err) => toast.error(describeError(err)),
  })

  const statusLabel = (s: number) => {
    if (s === CODE_STATUS.used) return t('admin:codeUsed')
    if (s === CODE_STATUS.disabled) return t('common:disabled')
    return t('admin:codeUnused')
  }

  const rows = codes.data?.data ?? []

  return (
    <div className="flex h-full min-h-0 flex-col gap-4">
      <PageHeader
        className="shrink-0"
        icon={Ticket}
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
        className="shrink-0"
        filters={
          <div className="flex items-center gap-2">
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
      {dialog}

      {codes.isError ? (
        <ErrorState message={describeError(codes.error)} onRetry={() => void codes.refetch()} />
      ) : codes.isPending ? (
        <TableSkeleton rows={6} cols={8} />
      ) : rows.length === 0 ? (
        <EmptyState
          hint={t('admin:codesEmptyHint')}
          action={
            <Button onClick={() => setDrawer(true)}>
              <Plus className="h-4 w-4" />
              {t('admin:redeemGenerate')}
            </Button>
          }
        />
      ) : (
        <Table
          stickyHeader
          wrapperClassName="min-h-40 max-h-none flex-1 overscroll-contain [scrollbar-gutter:stable]"
          scrollResetKey={`${status}:${pager.offset}:${pager.limit}`}
          aria-busy={codes.isFetching}
        >
          <THead>
            <Tr>
              <Th>ID</Th>
              <Th>{t('admin:codeBatch')}</Th>
              <Th numeric>{t('admin:codeFaceValue')}</Th>
              <Th>{t('common:status')}</Th>
              <Th>{t('admin:codePlan')}</Th>
              <Th>{t('admin:codeCreated')}</Th>
              <Th>{t('admin:codeRedeemedBy')}</Th>
              <Th>{t('admin:codeRedeemedAt')}</Th>
              <Th>{t('common:actions')}</Th>
            </Tr>
          </THead>
          <TBody>
            {rows.map((c) => (
              <Tr key={c.id}>
                <Td>{c.id}</Td>
                <Td className="font-mono text-xs">{c.batch_id.slice(0, 8)}…</Td>
                <Td numeric>{formatMoney(c.amount_micro, i18n.language)}</Td>
                <Td>
                  <Badge variant={c.status === CODE_STATUS.unused ? 'success' : 'muted'}>
                    {statusLabel(c.status)}
                  </Badge>
                </Td>
                <Td>{c.plan_code ?? '—'}</Td>
                <Td className="whitespace-nowrap text-xs">{dayjs(c.created_at).format('MM-DD HH:mm')}</Td>
                <Td>{c.redeemed_by ?? '—'}</Td>
                <Td className="whitespace-nowrap text-xs">
                  {c.redeemed_at ? dayjs(c.redeemed_at).format('MM-DD HH:mm') : '—'}
                </Td>
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

      <Pagination {...pager} total={codes.data?.total} className="shrink-0" />

      {drawer && <GenerateDrawer onClose={() => setDrawer(false)} onDone={invalidate} />}
    </div>
  )
}
