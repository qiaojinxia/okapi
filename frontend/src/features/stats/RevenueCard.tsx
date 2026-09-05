import { FreshnessNotice } from '@/features/analytics/FreshnessNotice'
import { TimeChart } from '@/components/ui/time-chart'
import { EmptyState, ErrorState, LoadingState } from '@/components/ui/state'
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
import { calendarDays } from '@/features/portal-overview/usage-chart-data'

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
  // 成本采集上线前的历史行成本恒 0：窗口内一笔成本都没有就不画成本柱、不出毛利
  const hasCost = (total?.cost_known_requests ?? 0) > 0
  const source = new Map((q.data?.data ?? []).map((row) => [row.day, row]))
  const dates = q.data?.window ? calendarDays(q.data.window.start_date, q.data.window.end_date) : [...source.keys()].sort()
  const chart = dates.map((bucket) => ({
    bucket,
    amount: (source.get(bucket)?.amount_micro ?? 0) / 1_000_000,
    discount: (source.get(bucket)?.discount_micro ?? 0) / 1_000_000,
    cost: (source.get(bucket)?.cost_known_requests ?? 0) > 0 ? (source.get(bucket)?.known_cost_micro ?? 0) / 1_000_000 : null,
  }))

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('admin:statRevenue')}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        <p className="text-xs text-muted-foreground">{t('admin:statRevenueHint')}</p>
        {q.isError ? (
          <ErrorState message={describeError(q.error)} onRetry={() => void q.refetch()} />
        ) : q.isPending ? <LoadingState /> : !q.data?.data.length ? <EmptyState hint={t('admin:trendEmptyHint')} /> : (
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
              {hasCost ? (
                <>
                  <Badge variant="muted">
                    {t('admin:statUpstreamCost')} {formatMoney(total?.known_cost_micro ?? 0, i18n.language)}
                  </Badge>
                  <Badge variant={(total?.known_margin_micro ?? 0) < 0 ? 'destructive' : 'success'}>
                    {t('analysis:coveredMargin')} {formatMoney(total?.known_margin_micro ?? 0, i18n.language)} · {t('analysis:coverage', { v: formatBp(total?.cost_coverage_bp ?? 0, i18n.language) })}
                  </Badge>
                </>
              ) : (
                <Badge variant="muted" title={t('admin:statMarginPendingHint')}>
                  {t('admin:statMarginPending')}
                </Badge>
              )}
            </div>
            <p className="text-xs text-muted-foreground">{t('analysis:costHint')}</p>
            <FreshnessNotice value={q.data?.window?.freshness} />
            <CashflowRow days={days} />
            <TimeChart key={String(hasCost)} label={t('admin:statRevenue')} data={chart} unit="USD" defaultType="bar" format={(value) => formatMoney(Math.round(value * 1_000_000), i18n.language)} series={[
              { key: 'amount', label: t('admin:statAmount'), color: 'var(--color-primary)' },
              { key: 'discount', label: t('admin:statDiscount'), color: 'var(--color-chart-2)' },
              ...(hasCost ? [{ key: 'cost', label: t('admin:statUpstreamCost'), color: 'var(--color-warning)' }] : []),
            ]} />
            {q.data?.window && <p className="text-xs text-muted-foreground">{q.data.window.start_date} — {q.data.window.end_date} · {q.data.window.timezone}</p>}
            {/* 阅读顺序：合计徽章 → 资金流入 → 按日趋势 → 分组下钻表；表在图后，图才不被挤到底 */}
            <GroupsTable days={days} />
          </>
        )}
      </CardContent>
    </Card>
  )
}
