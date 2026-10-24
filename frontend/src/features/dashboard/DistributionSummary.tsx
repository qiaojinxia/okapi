import { useId } from 'react'
import { useQuery } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { useTranslation } from 'react-i18next'
import { ErrorState, LoadingState } from '@/components/ui/state'
import { Tooltip } from '@/components/ui/tooltip'
import { Tabs } from '@/components/ui/tabs'
import type { BreakdownResp } from '@/features/analytics/types'
import { describeError } from '@/lib/i18n'
import { formatBp, formatCount, formatMoney, formatMoneyAggregate } from '@/lib/money'
import { qk } from '@/lib/query-keys'
import { DASHBOARD_RANKING_LIMIT, DASHBOARD_STALE_TIME, dashboardBreakdownParams, dashboardFetch } from './data'
import type { DistributionView, RankingMetric } from './types'
import { InventoryGrid } from './InventoryGrid'
import { ShareBar } from './ShareBar'
import { TokenSummary } from './TokenSummary'
import type { DateRange } from '@/components/ui/date-range'
import { dashboardPeriodLabel, dashboardSearch } from './period'
import { HEAD_LINK } from './distribution-layout'

function Ranking({ days, range, by, metric, onMetricChange }: { days: number; range?: DateRange | null; by: 'model' | 'channel'; metric: RankingMetric; onMetricChange: (metric: RankingMetric) => void }) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const params = dashboardBreakdownParams(days, by, metric, range)
  const search = dashboardSearch(days, range)
  const query = useQuery({ queryKey: qk.statsBreakdown(params), queryFn: () => dashboardFetch<BreakdownResp>(`/admin/stats/breakdown?${params}`), staleTime: DASHBOARD_STALE_TIME, retry: false })
  const title = metric === 'amount' ? t(by === 'model' ? 'portal:modelSnapshot' : 'admin:dashboardChannelRanking') : t(`admin:dashboardRanking_${by}_${metric}`)
  const data = query.isError ? undefined : query.data
  const rows = data?.data ?? []
  const visible = rows.slice(0, DASHBOARD_RANKING_LIMIT)
  const total = metric === 'amount' ? data?.total_amount_micro : metric === 'requests' ? data?.total_requests : data?.total_tokens
  const format = (value: number) => metric === 'amount' ? formatMoney(value, locale) : metric === 'requests' ? t('admin:dashboardRankingCalls', { n: formatCount(value, locale) }) : `${formatCount(value, locale)} Tokens`
  const totalText = total == null ? '—' : metric === 'amount' ? formatMoneyAggregate(total, locale) : format(total)
  const shares = visible.map((row) => metric === 'amount' ? row.share_bp : metric === 'requests' ? row.request_share_bp : row.token_share_bp)
  const topShare = total != null && total > 0 && shares.every((share) => share != null) ? Math.min(10000, shares.reduce((sum, share) => sum + Math.max(0, share!), 0)) : null
  const rowKey = (row: (typeof rows)[number]) => `${row.rank}:${row.key}`
  const nameOf = (row: (typeof rows)[number]) => row.label || (by === 'model' ? row.key || t('analysis:notCollected') : (row.channel_id ?? 0) > 0 ? t('admin:dashboardUnnamedChannel', { id: row.channel_id }) : t('admin:dashboardUnassignedChannel'))
  return <div role="region" aria-label={title} className="flex min-w-0 flex-col gap-3">
    <div className="flex flex-wrap items-baseline justify-between gap-x-2 gap-y-1">
      <div className="flex min-w-0 flex-col gap-0.5">
        <span className="flex flex-wrap items-center text-xs text-muted-foreground">
          {dashboardPeriodLabel(days, range, t)} ·
          <select aria-label={t('admin:dashboardRankingSort', { by: t(by === 'model' ? 'admin:invModels' : 'admin:invChannels') })} value={metric}
            onChange={(event) => onMetricChange(event.target.value as RankingMetric)} className="min-h-8 min-w-0 rounded border border-transparent bg-transparent px-1 text-xs text-muted-foreground outline-none hover:bg-muted focus-visible:border-primary focus-visible:ring-2 focus-visible:ring-primary/40 lg:min-h-6">
            {(['amount', 'requests', 'tokens'] as const).map((value) => <option key={value} value={value}>{t(`charts:metric_${value}`)}</option>)}
          </select>
        </span>
        <Tooltip content={t('admin:dashboardRankingDenominator')}>
          <span tabIndex={0} className="rounded text-lg leading-7 font-bold tabular-nums outline-none focus-visible:ring-2 focus-visible:ring-primary/40">{totalText}</span>
        </Tooltip>
      </div>
      <Link to="/admin/stats" search={{ ...search, view: 'breakdown', by, metric: metric === 'amount' ? undefined : metric }} className={HEAD_LINK}>{t('admin:dashboardViewAll')}<span aria-hidden>&nbsp;→</span></Link>
    </div>
    {query.isError ? <ErrorState message={describeError(query.error)} onRetry={() => void query.refetch()} /> : query.isPending ? <LoadingState /> : rows.length === 0 ? <p className="py-6 text-xs text-muted-foreground">{t('admin:trendEmptyHint')}</p> : <>
      <ol aria-label={title} className="flex flex-col gap-3">
        {visible.map((row, index) => {
          const name = nameOf(row)
          const value = metric === 'amount' ? row.amount_micro : row[metric]
          const share = total != null && total > 0 && shares[index] != null ? Math.max(0, Math.min(10000, shares[index]!)) : null
          const displayValue = value == null ? '—' : format(value)
          const percent = share === null ? '—' : formatBp(share, locale)
          const content = <>
            <span className="flex items-baseline gap-2 text-[13px]">
              <span className="min-w-0 flex-1 truncate font-medium" title={name}>{name}</span>
              <span className="shrink-0 text-xs text-muted-foreground tabular-nums">{displayValue}</span>
              <span className="w-[52px] shrink-0 text-right font-semibold tabular-nums">{percent}</span>
            </span>
            <ShareBar bp={share} opacity={1 - index * 0.2} />
          </>
          const className = 'flex w-full flex-col gap-1.5 rounded-md px-1.5 py-1'
          const canFocus = by === 'model' ? !!row.key : (row.channel_id ?? 0) > 0
          const description = `${name} · ${t('admin:dashboardRankingRequests', { n: formatCount(row.requests, locale) })}`
          return <li key={rowKey(row)} className="-mx-1.5 min-w-0">{canFocus
            ? <Link to="/admin/stats" title={description} aria-label={`${description} · ${displayValue} · ${percent}`} search={{ ...search, measure: metric === 'amount' ? undefined : metric, ...(by === 'model' ? { model: row.key } : { channel_id: row.channel_id }) }} className={`${className} outline-none hover:bg-muted/60 focus-visible:ring-2 focus-visible:ring-primary/40`}>{content}</Link>
            : <div title={description} className={className}>{content}</div>}</li>
        })}
      </ol>
      <p className="text-xs text-muted-foreground/80 tabular-nums">{t('admin:dashboardDistributionTopShare', { count: visible.length, value: topShare == null ? '—' : formatBp(topShare, locale) })}</p>
    </>}
  </div>
}

/// 用量分布：模型 / 渠道 / Token 构成三个页签共用一张卡，底部是站点规模的 2×2 速览。
/// 三个视图的查询都保持挂载（隐藏的面板不重复请求），切签只换显示。
export function DistributionSummary({ days, range, view, onViewChange, modelMetric, channelMetric, onMetricChange }: { days: number; range?: DateRange | null; view: DistributionView; onViewChange: (view: DistributionView) => void; modelMetric: RankingMetric; channelMetric: RankingMetric; onMetricChange: (by: 'model' | 'channel', metric: RankingMetric) => void }) {
  const { t } = useTranslation()
  const tabsId = useId()
  const views: DistributionView[] = ['model', 'channel', 'tokens']
  return <section data-slot="dashboard-distribution" role="region" aria-label={t('admin:dashboardDistribution')} className="flex min-w-0 flex-[1_1_20rem] flex-col gap-3 rounded-xl border border-border bg-card p-3.5">
    <div className="flex flex-wrap items-center gap-3">
      <h2 className="flex min-h-[34px] items-center text-[15px] font-semibold">{t('admin:dashboardDistribution')}</h2>
      <span className="flex-1" />
      <Tabs id={tabsId} className="md:[&_button]:min-h-6 md:[&_button]:text-xs" ariaLabel={t('admin:dashboardDistributionViews')} active={view} onChange={(next) => onViewChange(next as DistributionView)} items={views.map((id) => ({ id, label: t(`admin:dashboardDistribution_${id}`), panelId: `${tabsId}-${id}-panel` }))} />
    </div>
    {views.map((id) => <div key={id} id={`${tabsId}-${id}-panel`} role="tabpanel" aria-labelledby={`${tabsId}-${id}`} hidden={view !== id} className="min-w-0">
      {id === 'tokens' ? <TokenSummary days={days} range={range} /> : <Ranking days={days} range={range} by={id} metric={id === 'model' ? modelMetric : channelMetric} onMetricChange={(metric) => onMetricChange(id, metric)} />}
    </div>)}
    <span className="flex-1" />
    <InventoryGrid />
  </section>
}
