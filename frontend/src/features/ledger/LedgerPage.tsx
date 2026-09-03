import { useInfiniteQuery } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import dayjs from 'dayjs'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Receipt } from 'lucide-react'
import { PageHeader } from '@/components/ui/page'
import { CopyText } from '@/components/ui/copy-button'
import { TableSkeleton } from '@/components/ui/skeleton'
import { EmptyState, ErrorState } from '@/components/ui/state'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { Tabs } from '@/components/ui/tabs'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatMoney } from '@/lib/money'
import { qk } from '@/lib/query-keys'

const TABS = ['ledger', 'orders'] as const
type Tab = (typeof TABS)[number]
const PAGE = 50

/// 账户流水：钱怎么来、怎么被动过（与日志页"钱怎么花"互补）。
///
/// 两个页签而非一张混合表：余额变动是**已发生的账**，充值订单含**未支付/失败**
/// 的单——"我付了钱怎么没到账"要看的是订单状态；把未支付单混进流水会让
/// 余额列对不上。
export function LedgerPage() {
  const { t } = useTranslation()
  const [tab, setTab] = useState<Tab>('ledger')
  return (
    <div className="flex flex-col gap-4">
      <PageHeader title={t('portal:ledgerNav')} description={t('portal:ledgerDesc')} icon={Receipt} />
      <Tabs
        items={TABS.map((id) => ({
          id,
          label: id === 'ledger' ? t('portal:ledgerTabEvents') : t('portal:ledgerTabOrders'),
        }))}
        active={tab}
        onChange={(id) => setTab(id as Tab)}
      />
      {tab === 'ledger' ? <LedgerTable /> : <OrdersTable />}
    </div>
  )
}

interface LedgerRow {
  event_id: number
  event_type: 'recharge' | 'adjust' | 'refund' | 'expire'
  delta_micro: number
  balance_after_micro: number | null
  source: 'payment' | 'redeem' | 'aff' | 'admin' | 'expiry' | 'migration' | 'system'
  tags: string[]
  request_id: string | null
  created_at: string
}

interface Page<T> {
  data: T[]
  next_before: number | null
}

function usePaged<T>(key: readonly unknown[], path: string) {
  return useInfiniteQuery({
    queryKey: key,
    queryFn: ({ pageParam }) =>
      apiFetch<Page<T>>(`${path}?limit=${PAGE}${pageParam === null ? '' : `&before=${pageParam}`}`),
    initialPageParam: null as number | null,
    getNextPageParam: (last) => (last.data.length < PAGE ? null : last.next_before),
  })
}

function LoadMore({
  hasNext,
  fetching,
  onMore,
}: {
  hasNext: boolean
  fetching: boolean
  onMore: () => void
}) {
  const { t } = useTranslation()
  if (!hasNext) return null
  return (
    <Button variant="outline" className="self-center" disabled={fetching} onClick={onMore}>
      {fetching ? t('common:loading') : t('portal:logsLoadMore')}
    </Button>
  )
}

function LedgerTable() {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const q = usePaged<LedgerRow>(qk.myLedger, '/api/me/ledger')
  if (q.isError) return <ErrorState message={describeError(q.error)} />
  if (q.isPending) return <TableSkeleton rows={6} cols={5} />
  const rows = q.data.pages.flatMap((p) => p.data)
  if (rows.length === 0) return <EmptyState hint={t('portal:ledgerEmptyHint')} />

  return (
    <div className="flex flex-col gap-3">
      <Table>
        <THead>
          <Tr>
            <Th>{t('logs:time')}</Th>
            <Th>{t('portal:ledgerSource')}</Th>
            <Th numeric>{t('portal:ledgerDelta')}</Th>
            <Th numeric>{t('portal:ledgerBalanceAfter')}</Th>
            <Th>{t('common:description')}</Th>
          </Tr>
        </THead>
        <TBody>
          {rows.map((r) => (
            <Tr key={r.event_id}>
              <Td className="whitespace-nowrap text-xs">
                {dayjs(r.created_at).format('YYYY-MM-DD HH:mm')}
              </Td>
              <Td>
                <Badge dot variant={sourceVariant(r)}>{t(`portal:ledgerSource_${r.source}`)}</Badge>
              </Td>
              {/* 进账绿、出账默认色：符号 + 颜色双编码，色弱用户也读得出方向 */}
              <Td numeric className={r.delta_micro > 0 ? 'font-medium text-success' : 'font-medium'}>
                {r.delta_micro > 0 ? '+' : ''}
                {formatMoney(r.delta_micro, locale)}
              </Td>
              <Td numeric className="text-xs text-muted-foreground">
                {r.balance_after_micro === null ? '—' : formatMoney(r.balance_after_micro, locale)}
              </Td>
              <Td className="text-xs">
                {describe(r, t)}
                {r.request_id && (
                  <>
                    {' '}
                    <Link
                      to="/portal/logs"
                      className="font-mono text-muted-foreground underline decoration-dotted"
                      title={r.request_id}
                    >
                      {r.request_id.slice(0, 8)}…
                    </Link>
                  </>
                )}
              </Td>
            </Tr>
          ))}
        </TBody>
      </Table>
      <LoadMore
        hasNext={q.hasNextPage}
        fetching={q.isFetchingNextPage}
        onMore={() => void q.fetchNextPage()}
      />
    </div>
  )
}

function sourceVariant(r: LedgerRow): 'success' | 'muted' | 'destructive' {
  if (r.event_type === 'expire') return 'destructive'
  if (r.delta_micro > 0) return 'success'
  return 'muted'
}

/// 描述列：事件类型 + 标签的组合文案。标签是运营打的结构化标记
/// （compensation / goodwill / correction / aff_rebate…），有对应文案的翻译、
/// 没有的原样显示——新标签不必先改前端。
function describe(r: LedgerRow, t: (k: string, o?: Record<string, unknown>) => string): string {
  const typeText = t(`portal:ledgerType_${r.event_type}`)
  const tags = r.tags
    .filter((tag) => tag !== r.event_type && tag !== 'recharge' && tag !== 'redeem')
    .map((tag) => t(`portal:ledgerTag_${tag}`, { defaultValue: tag }))
  return tags.length > 0 ? `${typeText} · ${tags.join(' · ')}` : typeText
}

interface OrderRow {
  id: number
  order_no: string
  amount_micro: number
  currency: string
  pay_amount: string | null
  gateway: string
  status: 0 | 1 | 2 | 3
  paid_at: string | null
  created_at: string
}

const ORDER_STATUS: Record<OrderRow['status'], { key: string; variant: 'muted' | 'success' | 'destructive' }> = {
  0: { key: 'created', variant: 'muted' },
  1: { key: 'paid', variant: 'success' },
  2: { key: 'failed', variant: 'destructive' },
  3: { key: 'refunded', variant: 'muted' },
}

function OrdersTable() {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const q = usePaged<OrderRow>(qk.myOrders, '/api/me/orders')
  if (q.isError) return <ErrorState message={describeError(q.error)} />
  if (q.isPending) return <TableSkeleton rows={6} cols={5} />
  const rows = q.data.pages.flatMap((p) => p.data)
  if (rows.length === 0) return <EmptyState hint={t('portal:ordersEmptyHint')} />

  return (
    <div className="flex flex-col gap-3">
      <Table>
        <THead>
          <Tr>
            <Th>{t('logs:time')}</Th>
            <Th>{t('portal:orderNo')}</Th>
            <Th numeric>{t('portal:orderCredit')}</Th>
            <Th>{t('portal:orderPaid')}</Th>
            <Th>{t('portal:orderGateway')}</Th>
            <Th>{t('common:status')}</Th>
          </Tr>
        </THead>
        <TBody>
          {rows.map((r) => {
            const st = ORDER_STATUS[r.status]
            return (
              <Tr key={r.id}>
                <Td className="whitespace-nowrap text-xs">
                  {dayjs(r.created_at).format('YYYY-MM-DD HH:mm')}
                </Td>
                <Td>
                  <CopyText value={r.order_no} />
                </Td>
                <Td numeric>{formatMoney(r.amount_micro, locale)}</Td>
                {/* 原币种金额是后端 NUMERIC 文本，直接拼货币码，不经浮点 */}
                <Td className="text-xs">{r.pay_amount ? `${r.pay_amount} ${r.currency}` : '—'}</Td>
                <Td>
                  <Badge variant="muted">{r.gateway}</Badge>
                </Td>
                <Td>
                  <Badge dot variant={st.variant}>{t(`portal:orderStatus_${st.key}`)}</Badge>
                </Td>
              </Tr>
            )
          })}
        </TBody>
      </Table>
      <LoadMore
        hasNext={q.hasNextPage}
        fetching={q.isFetchingNextPage}
        onMore={() => void q.fetchNextPage()}
      />
    </div>
  )
}
