import { useId } from 'react'
import { useQuery } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { ArrowUpRight } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { ErrorState, LoadingState } from '@/components/ui/state'
import { Tooltip } from '@/components/ui/tooltip'
import { Tabs } from '@/components/ui/tabs'
import type { BreakdownResp } from '@/features/analytics/types'
import { chartColor } from '@/lib/chart'
import { describeError } from '@/lib/i18n'
import { formatBp, formatCount, formatMoney } from '@/lib/money'
import { qk } from '@/lib/query-keys'
import { DASHBOARD_RANKING_LIMIT, DASHBOARD_STALE_TIME, dashboardBreakdownParams, dashboardFetch } from './data'
import type { DistributionView, RankingMetric } from './types'
import { TokenSummary } from './TokenSummary'
import type { DateRange } from '@/components/ui/date-range'
import { dashboardPeriodLabel, dashboardSearch } from './period'

const detailClass = 'inline-flex min-h-8 shrink-0 items-center gap-1 rounded text-xs text-primary outline-none hover:underline focus-visible:ring-2 focus-visible:ring-primary/40 lg:min-h-6'

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
  const totalLabel = metric === 'amount' ? t('admin:dashboardAllSpend', { amount: total == null ? '—' : formatMoney(total, locale) }) : t('admin:dashboardRankingAll', { value: total == null ? '—' : format(total) })
  const shares = visible.map((row) => metric === 'amount' ? row.share_bp : metric === 'requests' ? row.request_share_bp : row.token_share_bp)
  const topShare = total != null && total > 0 && shares.every((share) => share != null) ? Math.min(10000, shares.reduce((sum, share) => sum + Math.max(0, share!), 0)) : null
  return <div role="region" aria-label={title} className="flex h-full min-w-0 flex-col">
    <CardHeader className="shrink-0 gap-0 px-4 pt-1 pb-2 lg:py-0">
      <CardTitle className="lg:sr-only">{title}</CardTitle>
      <div className="flex min-w-0 flex-wrap items-center justify-between gap-x-1 text-xs text-muted-foreground">
        <span>{t('admin:dashboardRankingPeriod', { period: dashboardPeriodLabel(days, range, t), count: DASHBOARD_RANKING_LIMIT })}</span>
        <div className="flex items-center gap-2">
        <select aria-label={t('admin:dashboardRankingSort', { by: t(by === 'model' ? 'admin:invModels' : 'admin:invChannels') })} value={metric}
          onChange={(event) => onMetricChange(event.target.value as RankingMetric)} className="min-h-8 min-w-0 rounded border border-transparent bg-transparent px-1 text-xs text-foreground outline-none hover:bg-muted focus-visible:border-primary focus-visible:ring-2 focus-visible:ring-primary/40 lg:min-h-6">
          {(['amount', 'requests', 'tokens'] as const).map((value) => <option key={value} value={value}>{t(`charts:metric_${value}`)}</option>)}
        </select>
        <Link to="/admin/stats" search={{ ...search, view: 'breakdown', by, metric: metric === 'amount' ? undefined : metric }} className={detailClass}>{t('admin:dashboardViewAll')}<ArrowUpRight aria-hidden size={13} /></Link>
        </div>
      </div>
    </CardHeader>
    <CardContent className="flex flex-1 flex-col gap-3 px-4 pt-0 pb-3 lg:gap-1.5 lg:pb-2">
      {query.isError ? <ErrorState message={describeError(query.error)} onRetry={() => void query.refetch()} /> : query.isPending ? <LoadingState /> : rows.length === 0 ? <p className="px-1 py-6 text-xs text-muted-foreground">{t('admin:trendEmptyHint')}</p> : <>
        {/* Shares include the full window, not just the displayed ranking. Reserve equal slots without fabricating missing rows. */}
        <ol data-slot="distribution-bars" aria-label={t('admin:dashboardDistributionChart', { label: title })} className="grid flex-1 [--ranking-row-min:3.5rem] lg:[--ranking-row-min:2rem]" style={{ gridTemplateRows: `repeat(${DASHBOARD_RANKING_LIMIT}, minmax(var(--ranking-row-min), 1fr))` }}>
          {visible.map((row) => {
            const name = row.label || (by === 'model' ? row.key || t('analysis:notCollected') : (row.channel_id ?? 0) > 0 ? t('admin:dashboardUnnamedChannel', { id: row.channel_id }) : t('admin:dashboardUnassignedChannel'))
            const value = metric === 'amount' ? row.amount_micro : row[metric]
            const proportion = metric === 'amount' ? row.share_bp : metric === 'requests' ? row.request_share_bp : row.token_share_bp
            const share = total != null && total > 0 && proportion != null ? Math.max(0, Math.min(10000, proportion)) : null
            const displayValue = value == null ? '—' : format(value)
            const content = <>
              <span className={`flex h-5 w-5 shrink-0 items-center justify-center rounded-md text-[11px] font-semibold tabular-nums ${row.rank === 1 ? 'bg-primary/12 text-primary' : 'bg-muted text-muted-foreground'}`}>{row.rank}</span>
              <span className="min-w-0 flex-1 space-y-1 lg:space-y-0">
                <span className="flex min-w-0 items-baseline justify-between gap-2">
                  <span className="min-w-0 truncate text-sm font-medium lg:text-xs lg:leading-4" title={name}>{name}</span>
                  <span className="shrink-0 text-[11px] leading-4 tabular-nums text-muted-foreground" title={t('admin:dashboardRankingRequests', { n: row.requests.toLocaleString(locale) })}>{t('admin:dashboardRankingCalls', { n: formatCount(row.requests, locale) })}</span>
                </span>
                <span className="grid grid-cols-[6rem_minmax(0,1fr)_3rem] items-center gap-2 text-xs text-muted-foreground lg:text-[11px] lg:leading-3.5">
                  <span className="min-w-0 truncate font-medium tabular-nums text-foreground" title={displayValue}>{displayValue}</span>
                  <span className="block h-1.5 overflow-hidden rounded-full bg-muted/50" aria-hidden style={{ backgroundImage: 'linear-gradient(to right, var(--color-border) 1px, transparent 1px)', backgroundSize: '25% 100%' }}><span data-slot="distribution-bar" data-share-bp={share ?? undefined} className="block h-full rounded-full" style={{ width: `${(share ?? 0) / 100}%`, background: chartColor(row.rank - 1) }} /></span>
                  <span className="min-w-9 text-right tabular-nums">{share === null ? '—' : formatBp(share, locale)}</span>
                </span>
              </span>
            </>
            const className = 'flex h-full min-h-14 items-center gap-2 rounded-lg px-1 py-1 lg:min-h-8 lg:py-px'
            const canFocus = by === 'model' ? !!row.key : (row.channel_id ?? 0) > 0
            const description = `${name} · ${t('admin:dashboardRankingRequests', { n: formatCount(row.requests, locale) })}`
            return <li key={row.key} className="min-w-0">{canFocus ? <Link to="/admin/stats" title={description} aria-label={`${description} · ${displayValue} · ${share === null ? '—' : formatBp(share, locale)}`} search={{ ...search, measure: metric === 'amount' ? undefined : metric, ...(by === 'model' ? { model: row.key } : { channel_id: row.channel_id }) }} className={`${className} outline-none hover:bg-muted/60 focus-visible:ring-2 focus-visible:ring-primary/40`}>{content}</Link> : <div title={description} className={className}>{content}</div>}</li>
          })}
        </ol>
        <div aria-hidden className="ml-8 flex justify-between px-1 text-[10px] tabular-nums text-muted-foreground lg:hidden"><span>0%</span><span>50%</span><span>100%</span></div>
        <div className="mt-auto flex flex-wrap items-center justify-between gap-x-2 gap-y-1 border-t border-border px-1 pt-2 lg:pt-1">
          <Tooltip content={t('admin:dashboardRankingDenominator')}>
            <span tabIndex={0} className="text-[11px] text-muted-foreground outline-none focus-visible:ring-2 focus-visible:ring-primary/40">{totalLabel}</span>
          </Tooltip>
          <span className="text-[11px] tabular-nums text-muted-foreground">{t('admin:dashboardDistributionTopShare', { count: visible.length, value: topShare == null ? '—' : formatBp(topShare, locale) })}</span>
        </div>
      </>}
    </CardContent>
  </div>
}

export function DistributionSummary({ days, range, view, onViewChange, modelMetric, channelMetric, onMetricChange }: { days: number; range?: DateRange | null; view: DistributionView; onViewChange: (view: DistributionView) => void; modelMetric: RankingMetric; channelMetric: RankingMetric; onMetricChange: (by: 'model' | 'channel', metric: RankingMetric) => void }) {
  const { t } = useTranslation()
  const tabsId = useId()
  const views: DistributionView[] = ['model', 'channel', 'tokens']
  return <Card data-slot="dashboard-distribution" role="region" aria-label={t('admin:dashboardDistribution')} className="flex min-w-0 flex-col rounded-xl lg:min-h-0">
    <CardHeader className="shrink-0 flex-row flex-wrap items-center justify-between gap-2 px-4 pt-2 pb-2">
      <CardTitle>{t('admin:dashboardDistribution')}</CardTitle>
      <Tabs id={tabsId} className="lg:[&_button]:min-h-7 lg:[&_button]:text-xs" ariaLabel={t('admin:dashboardDistributionViews')} active={view} onChange={(next) => onViewChange(next as DistributionView)} items={views.map((id) => ({ id, label: t(`admin:dashboardDistribution_${id}`), panelId: `${tabsId}-${id}-panel` }))} />
    </CardHeader>
    <div className="grid min-w-0 flex-1 lg:min-h-0 lg:grid-rows-[minmax(0,1fr)]">
      {/* Keep the two lightweight queries warm and share the existing usage query; tab changes never duplicate requests. */}
      {views.map((id) => <div key={id} id={`${tabsId}-${id}-panel`} role="tabpanel" aria-labelledby={`${tabsId}-${id}`} hidden={view !== id} className="min-w-0 lg:min-h-0 lg:overflow-y-auto">
        {id === 'tokens' ? <TokenSummary days={days} range={range} /> : <Ranking days={days} range={range} by={id} metric={id === 'model' ? modelMetric : channelMetric} onMetricChange={(metric) => onMetricChange(id, metric)} />}
      </div>)}
    </div>
  </Card>
}
