import { useQuery } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { ArrowUpRight } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { ErrorState, LoadingState } from '@/components/ui/state'
import { Tooltip } from '@/components/ui/tooltip'
import type { BreakdownResp } from '@/features/analytics/types'
import { chartColor } from '@/lib/chart'
import { describeError } from '@/lib/i18n'
import { formatBp, formatCount, formatMoney } from '@/lib/money'
import { qk } from '@/lib/query-keys'
import { DASHBOARD_RANKING_LIMIT, DASHBOARD_STALE_TIME, dashboardBreakdownParams, dashboardFetch } from './data'
import type { RankingMetric } from './types'
import { TokenSummary } from './TokenSummary'

const detailClass = 'inline-flex min-h-8 shrink-0 items-center gap-1 rounded text-xs text-primary outline-none hover:underline focus-visible:ring-2 focus-visible:ring-primary/40 lg:min-h-6'

function Ranking({ days, by, metric, onMetricChange }: { days: number; by: 'model' | 'channel'; metric: RankingMetric; onMetricChange: (metric: RankingMetric) => void }) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const params = dashboardBreakdownParams(days, by, metric)
  const query = useQuery({ queryKey: qk.statsBreakdown(params), queryFn: () => dashboardFetch<BreakdownResp>(`/admin/stats/breakdown?${params}`), staleTime: DASHBOARD_STALE_TIME, retry: false })
  const title = metric === 'amount' ? t(by === 'model' ? 'portal:modelSnapshot' : 'admin:dashboardChannelRanking') : t(`admin:dashboardRanking_${by}_${metric}`)
  const data = query.isError ? undefined : query.data
  const rows = data?.data ?? []
  const visible = rows.slice(0, DASHBOARD_RANKING_LIMIT)
  const total = metric === 'amount' ? data?.total_amount_micro : metric === 'requests' ? data?.total_requests : data?.total_tokens
  const format = (value: number) => metric === 'amount' ? formatMoney(value, locale) : metric === 'requests' ? t('admin:dashboardRankingCalls', { n: formatCount(value, locale) }) : `${formatCount(value, locale)} Tokens`
  const totalLabel = metric === 'amount' ? t('admin:dashboardAllSpend', { amount: formatMoney(total ?? 0, locale) }) : t('admin:dashboardRankingAll', { value: total == null ? '—' : format(total) })
  return <Card role="region" aria-label={title} className="flex min-w-0 flex-col rounded-xl lg:min-h-56">
    <CardHeader className="gap-0 px-3 pt-2 pb-1 lg:pt-1 lg:pb-0">
      <div className="flex flex-wrap items-center justify-between gap-x-2">
        <CardTitle>{title}</CardTitle>
        <Link to="/admin/stats" search={{ days, view: 'breakdown', by, metric: metric === 'amount' ? undefined : metric }} className={detailClass}>{t('admin:dashboardViewAll')}<ArrowUpRight aria-hidden size={13} /></Link>
      </div>
      <div className="flex min-w-0 flex-wrap items-center justify-between gap-x-1 text-xs text-muted-foreground">
        <span>{t('admin:dashboardRankingWindow', { days, count: DASHBOARD_RANKING_LIMIT })}</span>
        <select aria-label={t('admin:dashboardRankingSort', { by: t(by === 'model' ? 'admin:invModels' : 'admin:invChannels') })} value={metric}
          onChange={(event) => onMetricChange(event.target.value as RankingMetric)} className="min-h-8 min-w-0 rounded border border-transparent bg-transparent px-1 text-xs text-foreground outline-none hover:bg-muted focus-visible:border-primary focus-visible:ring-2 focus-visible:ring-primary/40 lg:min-h-6">
          {(['amount', 'requests', 'tokens'] as const).map((value) => <option key={value} value={value}>{t(`charts:metric_${value}`)}</option>)}
        </select>
      </div>
    </CardHeader>
    <CardContent className="flex flex-1 flex-col gap-1 px-2 pt-0 pb-2 lg:gap-0 lg:pb-1">
      {query.isError ? <ErrorState message={describeError(query.error)} onRetry={() => void query.refetch()} /> : query.isPending ? <LoadingState /> : rows.length === 0 ? <p className="px-1 py-6 text-xs text-muted-foreground">{t('admin:trendEmptyHint')}</p> : <>
        {/* Reserve three rows without inventing records when fewer models/channels have usage. */}
        <ol className="grid grid-rows-[repeat(3,minmax(3rem,auto))]">
          {visible.map((row) => {
            const name = row.label || (by === 'model' ? row.key || t('analysis:notCollected') : (row.channel_id ?? 0) > 0 ? t('admin:dashboardUnnamedChannel', { id: row.channel_id }) : t('admin:dashboardUnassignedChannel'))
            const value = metric === 'amount' ? row.amount_micro : row[metric]
            const proportion = metric === 'amount' ? row.share_bp : metric === 'requests' ? row.request_share_bp : row.token_share_bp
            const share = total != null && total > 0 && proportion != null ? Math.max(0, Math.min(10000, proportion)) : null
            const displayValue = value == null ? '—' : format(value)
            const content = <>
              <span className="mt-0.5 w-3 shrink-0 text-xs tabular-nums text-muted-foreground">{row.rank}</span>
              <span className="min-w-0 flex-1 space-y-1 lg:space-y-0.5">
                <span className="flex min-w-0 items-baseline justify-between gap-2">
                  <span className="min-w-0 truncate text-xs font-medium" title={name}>{name}</span>
                  <span className="shrink-0 text-[11px] leading-4 tabular-nums text-muted-foreground" title={t('admin:dashboardRankingRequests', { n: row.requests.toLocaleString(locale) })}>{t('admin:dashboardRankingCalls', { n: formatCount(row.requests, locale) })}</span>
                </span>
                <span className="flex items-center justify-between gap-2 text-[11px] text-muted-foreground">
                  <span className="min-w-0 break-all font-medium tabular-nums text-foreground">{displayValue}</span>
                  <span className="tabular-nums">{share === null ? '—' : formatBp(share, locale)}</span>
                </span>
                <span className="block h-1 overflow-hidden rounded-full bg-muted lg:h-0.5" aria-hidden><span className="block h-full rounded-full" style={{ width: `${(share ?? 0) / 100}%`, background: chartColor(row.rank - 1) }} /></span>
              </span>
            </>
            const className = 'flex min-h-12 items-start gap-1.5 rounded-lg px-1 py-1.5 lg:py-1'
            const canFocus = by === 'model' ? !!row.key : (row.channel_id ?? 0) > 0
            const description = `${name} · ${t('admin:dashboardRankingRequests', { n: formatCount(row.requests, locale) })}`
            return <li key={row.key} className="min-w-0">{canFocus ? <Link to="/admin/stats" title={description} aria-label={`${description} · ${displayValue} · ${share === null ? '—' : formatBp(share, locale)}`} search={{ days, measure: metric === 'amount' ? undefined : metric, ...(by === 'model' ? { model: row.key } : { channel_id: row.channel_id }) }} className={`${className} outline-none hover:bg-muted/60 focus-visible:ring-2 focus-visible:ring-primary/40`}>{content}</Link> : <div title={description} className={className}>{content}</div>}</li>
          })}
        </ol>
        <div className="mt-auto flex flex-wrap items-center justify-between gap-x-1 border-t border-border px-1 pt-1">
          <Tooltip content={t('admin:dashboardRankingDenominator')}>
            <span tabIndex={0} className="text-[11px] text-muted-foreground outline-none focus-visible:ring-2 focus-visible:ring-primary/40">{totalLabel}</span>
          </Tooltip>
        </div>
      </>}
    </CardContent>
  </Card>
}

export function DistributionSummary({ days, modelMetric, channelMetric, onMetricChange }: { days: number; modelMetric: RankingMetric; channelMetric: RankingMetric; onMetricChange: (by: 'model' | 'channel', metric: RankingMetric) => void }) {
  return <div className="grid min-w-0 items-stretch gap-2 sm:grid-cols-2 lg:grid-rows-[minmax(min-content,1fr)_auto]">
    <Ranking key={`model:${days}:${modelMetric}`} days={days} by="model" metric={modelMetric} onMetricChange={(metric) => onMetricChange('model', metric)} />
    <Ranking key={`channel:${days}:${channelMetric}`} days={days} by="channel" metric={channelMetric} onMetricChange={(metric) => onMetricChange('channel', metric)} />
    <TokenSummary days={days} />
  </div>
}
