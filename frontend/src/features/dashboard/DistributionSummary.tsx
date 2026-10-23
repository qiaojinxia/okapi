import { useId, useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { ArrowUpRight } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { PieChart } from '@/components/ui/pie-chart'
import type { PieSlice } from '@/components/ui/pie-chart'
import { ErrorState, LoadingState } from '@/components/ui/state'
import { Tooltip } from '@/components/ui/tooltip'
import { Tabs } from '@/components/ui/tabs'
import type { BreakdownResp } from '@/features/analytics/types'
import { chartColor, OTHER_COLOR } from '@/lib/chart'
import { describeError } from '@/lib/i18n'
import { formatBp, formatCount, formatMoney, formatMoneyAggregate } from '@/lib/money'
import { qk } from '@/lib/query-keys'
import { DASHBOARD_RANKING_LIMIT, DASHBOARD_STALE_TIME, dashboardBreakdownParams, dashboardFetch } from './data'
import type { DistributionView, RankingMetric } from './types'
import { TokenSummary } from './TokenSummary'
import type { DateRange } from '@/components/ui/date-range'
import { dashboardPeriodLabel, dashboardSearch } from './period'
import { BODY, HEAD, HEAD_LINK, HEAD_ROW, LEGEND, PANEL, PIE, STAGE } from './distribution-layout'

/// 「榜外」那一块的键（与行键不会冲突）。
const OTHER = '\u0000other'
/// 榜外占比低于 0.05% 时扇区只有一条看不见的细缝，不单画。
const OTHER_MIN_BP = 5

function Ranking({ days, range, by, metric, onMetricChange }: { days: number; range?: DateRange | null; by: 'model' | 'channel'; metric: RankingMetric; onMetricChange: (metric: RankingMetric) => void }) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const params = dashboardBreakdownParams(days, by, metric, range)
  const search = dashboardSearch(days, range)
  const [active, setActive] = useState<string | null>(null)
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
  const rest = topShare == null ? 0 : 10000 - topShare
  const rowKey = (row: (typeof rows)[number]) => `${row.rank}:${row.key}`
  const nameOf = (row: (typeof rows)[number]) => row.label || (by === 'model' ? row.key || t('analysis:notCollected') : (row.channel_id ?? 0) > 0 ? t('admin:dashboardUnnamedChannel', { id: row.channel_id }) : t('admin:dashboardUnassignedChannel'))
  // 扇区按占比画；占比未知（总量为 0、旧数据缺字段）时只画一圈灰底，不伪造
  const slices: PieSlice[] = topShare == null ? [] : [
    ...visible.map((row, i) => ({ key: rowKey(row), value: Math.max(0, shares[i]!), color: chartColor(row.rank - 1) })),
    ...(rest >= OTHER_MIN_BP ? [{ key: OTHER, value: rest, color: OTHER_COLOR }] : []),
  ]
  const focused = visible.find((row, i) => rowKey(row) === active && shares[i] != null)
  const centre = active === OTHER
    ? { top: t('analytics:other'), main: formatBp(rest, locale), sub: null }
    : focused
      ? { top: nameOf(focused), main: formatBp(Math.max(0, Math.min(10000, shares[visible.indexOf(focused)]!)), locale), sub: metric === 'requests' ? null : format((metric === 'amount' ? focused.amount_micro : focused[metric]) ?? 0) }
      : { top: t(`charts:metric_${metric}`), main: total == null ? '—' : metric === 'amount' ? formatMoneyAggregate(total, locale) : formatCount(total, locale), sub: null }
  const hover = (key: string) => ({ onMouseEnter: () => setActive(key), onMouseLeave: () => setActive(null), onFocus: () => setActive(key), onBlur: () => setActive(null) })
  return <div role="region" aria-label={title} className={PANEL}>
    <CardHeader className={HEAD}>
      <CardTitle className="lg:sr-only">{title}</CardTitle>
      <div data-slot="distribution-head" className={HEAD_ROW}>
        <span>{t('admin:dashboardRankingPeriod', { period: dashboardPeriodLabel(days, range, t), count: DASHBOARD_RANKING_LIMIT })}</span>
        <div className="flex items-center gap-2">
        <select aria-label={t('admin:dashboardRankingSort', { by: t(by === 'model' ? 'admin:invModels' : 'admin:invChannels') })} value={metric}
          onChange={(event) => onMetricChange(event.target.value as RankingMetric)} className="min-h-8 min-w-0 rounded border border-transparent bg-transparent px-1 text-xs text-foreground outline-none hover:bg-muted focus-visible:border-primary focus-visible:ring-2 focus-visible:ring-primary/40 lg:min-h-6">
          {(['amount', 'requests', 'tokens'] as const).map((value) => <option key={value} value={value}>{t(`charts:metric_${value}`)}</option>)}
        </select>
        <Link to="/admin/stats" search={{ ...search, view: 'breakdown', by, metric: metric === 'amount' ? undefined : metric }} className={HEAD_LINK}>{t('admin:dashboardViewAll')}<ArrowUpRight aria-hidden size={13} /></Link>
        </div>
      </div>
    </CardHeader>
    <CardContent className={BODY}>
      {query.isError ? <ErrorState message={describeError(query.error)} onRetry={() => void query.refetch()} /> : query.isPending ? <LoadingState /> : rows.length === 0 ? <p className="px-1 py-6 text-xs text-muted-foreground">{t('admin:trendEmptyHint')}</p> : <>
        {/* 大屏固定高度：本区是尺寸容器，扇形图按它的高 / 宽取较小者（cqh / cqw），图例在右侧垂直居中，放不下时图例内部滚动。
            窄屏自然高度：图在上、图例在下；够宽（容器 ≥ 28rem）时并排。 */}
        <div data-slot="distribution-body" className={STAGE}>
          <PieChart slot="distribution-pie" slices={slices} active={active} onActiveChange={setActive} label={t('admin:dashboardDistributionChart', { label: title })} hole={0.62}
            className={PIE}>
            <span className="max-w-full truncate text-[10px] leading-4 text-muted-foreground @[11rem]:text-[11px]" title={centre.top}>{centre.top}</span>
            <span className="max-w-full truncate text-xs leading-tight font-semibold tabular-nums @[10rem]:text-[13px] @[11rem]:text-base @[14rem]:text-xl">{centre.main}</span>
            {centre.sub && <span className="max-w-full truncate text-[10px] leading-4 tabular-nums text-muted-foreground @[11rem]:text-[11px]">{centre.sub}</span>}
          </PieChart>
          <div data-slot="distribution-legend" className={LEGEND}>
            <ol aria-label={title} className="flex min-w-0 flex-col gap-0.5 [@container(max-height:16rem)]:gap-0">
              {visible.map((row, index) => {
                const name = nameOf(row)
                const value = metric === 'amount' ? row.amount_micro : row[metric]
                const share = total != null && total > 0 && shares[index] != null ? Math.max(0, Math.min(10000, shares[index]!)) : null
                const displayValue = value == null ? '—' : format(value)
                const key = rowKey(row)
                const calls = t('admin:dashboardRankingCalls', { n: formatCount(row.requests, locale) })
                const content = <>
                  <span aria-hidden className="size-2.5 shrink-0 rounded-[3px]" style={{ background: chartColor(row.rank - 1) }} />
                  <span className="min-w-0 flex-1">
                    <span className="flex min-w-0 items-baseline justify-between gap-2">
                      <span className="min-w-0 truncate text-xs font-medium" title={name}>{name}</span>
                      {/* 图例放不下两行时（大屏固定高度、卡片矮）收成单行：金额挪到名称行，次数留在悬停提示里 */}
                      {metric !== 'requests' && <span className="hidden shrink-0 text-[11px] tabular-nums text-muted-foreground [@container(max-height:16rem)_and_(min-width:28rem)]:inline">{displayValue}</span>}
                      <span className="shrink-0 text-xs font-semibold tabular-nums">{share === null ? '—' : formatBp(share, locale)}</span>
                    </span>
                    <span className="mt-0.5 flex min-w-0 items-center gap-1.5 text-[11px] leading-4 tabular-nums text-muted-foreground [@container(max-height:16rem)]:hidden">
                      {metric !== 'requests' && <><span className="min-w-0 truncate" title={displayValue}>{displayValue}</span><span aria-hidden>·</span></>}
                      <span className="shrink-0" title={t('admin:dashboardRankingRequests', { n: row.requests.toLocaleString(locale) })}>{calls}</span>
                    </span>
                  </span>
                </>
                const className = `flex w-full items-center gap-2.5 rounded-lg px-2 py-1.5 transition-colors [@container(max-height:16rem)]:py-1 ${active === key ? 'bg-muted/70' : ''}`
                const canFocus = by === 'model' ? !!row.key : (row.channel_id ?? 0) > 0
                const description = `${name} · ${t('admin:dashboardRankingRequests', { n: formatCount(row.requests, locale) })}`
                return <li key={key} className="min-w-0" {...hover(key)}>{canFocus ? <Link to="/admin/stats" title={description} aria-label={`${description} · ${displayValue} · ${share === null ? '—' : formatBp(share, locale)}`} search={{ ...search, measure: metric === 'amount' ? undefined : metric, ...(by === 'model' ? { model: row.key } : { channel_id: row.channel_id }) }} className={`${className} outline-none hover:bg-muted/70 focus-visible:ring-2 focus-visible:ring-primary/40`}>{content}</Link> : <div title={description} className={className}>{content}</div>}</li>
              })}
            </ol>
            {slices.some((slice) => slice.key === OTHER) && <div {...hover(OTHER)} className={`flex items-center gap-2.5 rounded-lg px-2 py-1 text-[11px] text-muted-foreground transition-colors ${active === OTHER ? 'bg-muted/70' : ''}`}>
              <span aria-hidden className="size-2.5 shrink-0 rounded-[3px]" style={{ background: OTHER_COLOR }} />
              <span className="min-w-0 flex-1 truncate">{t('analytics:other')}</span>
              <span className="shrink-0 tabular-nums">{formatBp(rest, locale)}</span>
            </div>}
          </div>
        </div>
        <div className="mt-auto flex shrink-0 flex-wrap items-center justify-between gap-x-2 gap-y-1 rounded-xl border border-border/50 bg-muted/25 px-2.5 py-1.5 lg:py-[3px] lg:[@media(min-height:900px)]:py-1">
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
  return <Card data-slot="dashboard-distribution" role="region" aria-label={t('admin:dashboardDistribution')} className="flex min-w-0 flex-col rounded-2xl border-border/70 lg:min-h-0">
    <CardHeader className="shrink-0 flex-row flex-wrap items-center justify-between gap-2 px-5 pt-3 pb-2 lg:flex-nowrap lg:pt-2 lg:[@media(min-height:900px)]:pt-3">
      <CardTitle className="shrink-0 text-[15px] tracking-tight">{t('admin:dashboardDistribution')}</CardTitle>
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
