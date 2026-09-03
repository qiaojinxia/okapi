import { Bar, BarChart, CartesianGrid, ResponsiveContainer, Tooltip, XAxis, YAxis } from 'recharts'
import { useQuery } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import type { MarginResp } from '@/features/stats/types'
import { Badge } from '@/components/ui/badge'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatBp, formatCount, formatMoney } from '@/lib/money'
import { healthVariant } from '@/features/stats/ChannelHealthCard'
import { qk } from '@/lib/query-keys'

interface CashflowBucket {
  recharge_micro: number
  granted_micro: number
  clawed_micro: number
  expired_micro: number
}

/// 资金流入行（/admin/stats/cashflow，PG-only）。
///
/// 与下方消费图是一进一出的两面：图回答"用户花了多少"，这行回答"钱进来了
/// 多少"。充值与兑换入账分列——兑换码可能线下售出也可能是补偿，混成一个数
/// 会高估现金流。
function CashflowRow({ days }: { days: number }) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const q = useQuery({
    queryKey: qk.statsCashflow(days),
    queryFn: () =>
      apiFetch<{ today: CashflowBucket; window: CashflowBucket }>(
        `/admin/stats/cashflow?days=${days}`,
      ),
  })
  if (q.isError || !q.data) return null
  const w = q.data.window
  const item = (label: string, micro: number, tone?: 'success' | 'muted') => (
    <Badge variant={tone ?? 'muted'}>
      {label} {formatMoney(micro, locale)}
    </Badge>
  )
  return (
    <div className="flex flex-wrap items-center gap-2">
      <span className="text-xs text-muted-foreground">{t('admin:cashflowTitle')}</span>
      {item(t('admin:cashflowRecharge'), w.recharge_micro, 'success')}
      {item(t('admin:cashflowGranted'), w.granted_micro)}
      {w.clawed_micro > 0 && item(t('admin:cashflowClawed'), w.clawed_micro)}
      {w.expired_micro > 0 && item(t('admin:cashflowExpired'), w.expired_micro)}
    </div>
  )
}

interface GroupRow {
  group: string
  group_ratio: string | null
  requests: number
  tokens: number
  amount_micro: number
  share_bp: number
  discount_micro: number
  errors: number
  error_rate_bp: number
}

/// 分组经营表（/admin/stats/groups，mv_group_day 首个控制面出口）。
///
/// 价格分组是站长的商业分层：把倍率和收入占比摆在同一行，才能看出
/// "vip 打了 8 折却贡献六成收入"或"free 组占了一半请求量却在烧错误率"。
function GroupsTable({ days }: { days: number }) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const q = useQuery({
    queryKey: qk.statsGroups(days),
    queryFn: () => apiFetch<{ data: GroupRow[] }>(`/admin/stats/groups?days=${days}`),
  })
  const rows = q.data?.data ?? []
  if (q.isError || rows.length === 0) return null
  return (
    <div className="flex flex-col gap-2">
      <span className="text-xs text-muted-foreground">{t('admin:statByGroup')}</span>
      <Table>
        <THead>
          <Tr>
            <Th>{t('logs:group')}</Th>
            <Th>{t('admin:groupRatio')}</Th>
            <Th className="w-1/4">{t('admin:statErrorShare')}</Th>
            <Th>{t('admin:statAmount')}</Th>
            <Th>{t('admin:statDiscount')}</Th>
            <Th>{t('common:requests')}</Th>
            <Th>{t('admin:statErrorRate')}</Th>
          </Tr>
        </THead>
        <TBody>
          {rows.map((g) => (
            <Tr key={g.group}>
              <Td className="font-mono text-xs">{g.group}</Td>
              <Td className="text-xs">{g.group_ratio ? `×${g.group_ratio}` : '—'}</Td>
              <Td>
                <div className="flex items-center gap-2">
                  <div className="h-2 flex-1 overflow-hidden rounded bg-muted">
                    <div
                      className="h-full bg-primary/70"
                      style={{ width: `${Math.max(1, g.share_bp / 100)}%` }}
                    />
                  </div>
                  <span className="w-14 shrink-0 text-right text-xs">{formatBp(g.share_bp, locale)}</span>
                </div>
              </Td>
              <Td>{formatMoney(g.amount_micro, locale)}</Td>
              <Td>{formatMoney(g.discount_micro, locale)}</Td>
              <Td>{formatCount(g.requests, locale)}</Td>
              <Td>
                <Badge variant={healthVariant(g.error_rate_bp)}>{formatBp(g.error_rate_bp, locale)}</Badge>
              </Td>
            </Tr>
          ))}
        </TBody>
      </Table>
    </div>
  )
}

export function RevenueCard({ days }: { days: number }) {
  const { t, i18n } = useTranslation()
  const q = useQuery({
    queryKey: qk.statsMargin(days),
    queryFn: () => apiFetch<MarginResp>(`/admin/stats/margin?days=${days}`),
  })
  const total = q.data?.total
  const chart = (q.data?.data ?? []).map((d) => ({
    day: d.day.slice(5),
    amount: d.amount_micro / 1_000_000,
    discount: d.discount_micro / 1_000_000,
  }))

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('admin:statRevenue')}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        <p className="text-xs text-muted-foreground">{t('admin:statRevenueHint')}</p>
        {q.isError ? (
          <p className="text-sm text-destructive">{describeError(q.error)}</p>
        ) : (
          <>
            <div className="flex flex-wrap gap-2">
              <Badge variant="muted">
                {t('admin:statAmount')} {formatMoney(total?.amount_micro ?? 0, i18n.language)}
              </Badge>
              <Badge variant="muted">
                {t('admin:statDiscount')} {formatMoney(total?.discount_micro ?? 0, i18n.language)}
              </Badge>
              <Badge variant="muted">
                {t('common:requests')} {formatCount(total?.requests ?? 0, i18n.language)}
              </Badge>
              <Badge variant={healthVariant(total?.error_rate_bp ?? 0)}>
                {t('admin:statErrorRate')} {formatBp(total?.error_rate_bp ?? 0, i18n.language)}
              </Badge>
              {(total?.upstream_cost_micro ?? 0) > 0 ? (
                <Badge variant="success">
                  {t('admin:statMargin')} {formatMoney(total?.margin_micro ?? 0, i18n.language)}
                </Badge>
              ) : (
                <Badge variant="muted">{t('admin:statMarginPending')}</Badge>
              )}
            </div>
            <CashflowRow days={days} />
            <div className="h-64">
              <ResponsiveContainer width="100%" height="100%">
                <BarChart data={chart}>
                  <CartesianGrid strokeDasharray="3 3" className="stroke-border" />
                  <XAxis dataKey="day" fontSize={12} />
                  <YAxis fontSize={12} />
                  <Tooltip />
                  <Bar
                    dataKey="amount"
                    name={t('admin:statAmount')}
                    fill="var(--color-primary)"
                    isAnimationActive={false}
                  />
                  <Bar
                    dataKey="discount"
                    name={t('admin:statDiscount')}
                    fill="var(--color-muted-foreground)"
                    isAnimationActive={false}
                  />
                </BarChart>
              </ResponsiveContainer>
            </div>
            {/* 阅读顺序：合计徽章 → 资金流入 → 按日趋势 → 分组下钻表；表在图后，图才不被挤到底 */}
            <GroupsTable days={days} />
          </>
        )}
      </CardContent>
    </Card>
  )
}
