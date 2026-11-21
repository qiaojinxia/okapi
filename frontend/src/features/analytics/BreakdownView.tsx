import { useQuery } from '@tanstack/react-query'
import { useNavigate } from '@tanstack/react-router'
import { ArrowDown, ChevronDown, Crosshair, GitFork, ScrollText } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { AnalyticsSearch, BreakdownDim } from '@/features/analytics/route-state'
import { BREAKDOWN_DIMS, FLOW_METRICS, type FlowMetric } from '@/features/analytics/route-state'
import type { LogSearch } from '@/routes/admin.logs'
import { Badge } from '@/components/ui/badge'
import { Card } from '@/components/ui/card'
import { ActionGroup, IconButton } from '@/components/ui/icon-button'
import { ProviderMark } from '@/features/channels/ProviderChip'
import { ADVANCED_KEYS, compactSelectClass, dimensionLabel } from './AnalysisControls'
import { WorkspaceHeader } from './WorkspaceHeader'
import { EmptyState, ErrorState, LoadingState } from '@/components/ui/state'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { cleanSearch, logsWindow } from '@/features/analytics/search'
import { cubeParams, effectiveDays } from '@/lib/cube'
import type { BreakdownResp, BreakdownRow } from '@/lib/cube'
import { OTHER_COLOR, chartColor } from '@/lib/chart'
import { BAD_BP, WARN_BP } from '@/lib/health'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatBp, formatCount, formatMoney, formatMoneyAggregate, formatRatio } from '@/lib/money'
import { qk } from '@/lib/query-keys'
import { cn } from '@/lib/utils'
import { maskEmails } from '@/lib/mask'

const PRIMARY_DIMS: readonly BreakdownDim[] = ['model', 'channel', 'provider', 'user', 'api_key', 'group']
const FLOW_TRACE_DIMS = new Set<BreakdownDim>(['model', 'channel', 'user', 'api_key', 'group', 'node'])

/// 某维度已被过滤时，再按它拆分只会得到一行——该选项置灰。
function dimFiltered(dim: BreakdownDim, s: AnalyticsSearch): boolean {
  switch (dim) {
    case 'model':
      return s.model !== undefined
    case 'channel':
      return s.channel_id !== undefined
    case 'user':
      return s.user_id !== undefined
    case 'api_key':
      return s.api_key_id !== undefined
    case 'group':
      return s.group !== undefined
    default:
      return false
  }
}

/// 缺省拆分维度：URL 指定优先；否则取第一个未被过滤的维度。
export function effectiveBy(s: AnalyticsSearch): BreakdownDim {
  if (s.by !== undefined && !dimFiltered(s.by, s)) return s.by
  return BREAKDOWN_DIMS.find((d) => !dimFiltered(d, s)) ?? 'provider'
}

/// 毛利 = 实收 − 上游成本（官方价 × 渠道相对成本系数）；负毛利标红。
function MarginCell({ row, locale }: { row: BreakdownRow; locale: string }) {
  const { t } = useTranslation()
  const margin = row.known_margin_micro
  return (
    <Td numeric>
      <div className="flex flex-col items-end leading-tight">
        <span className={cn('font-medium tabular-nums', margin != null && margin < 0 && 'text-destructive')}>
          {margin == null ? '—' : formatMoney(margin, locale)}
        </span>
        <span className="mt-0.5 text-[11px] text-muted-foreground tabular-nums">
          {t('analysis:coverage', { v: row.cost_coverage_bp == null ? '—' : formatBp(row.cost_coverage_bp, locale) })}
        </span>
      </div>
    </Td>
  )
}

function RankChange({ row }: { row: BreakdownRow }) {
  const { t } = useTranslation()
  if (row.previous_rank === null) {
    return <span className="text-[10px] font-medium text-primary/80">{t('analytics:deltaNew')}</span>
  }
  const d = row.previous_rank - row.rank
  if (d === 0) return <span className="text-[10px] text-muted-foreground/65">—</span>
  return (
    <span className={cn('text-[10px] font-medium tabular-nums', d > 0 ? 'text-success' : 'text-destructive')}>
      {d > 0 ? `▲${d}` : `▼${-d}`}
    </span>
  )
}

function DeltaBadge({ bp, locale }: { bp: number | null; locale: string }) {
  const { t } = useTranslation()
  if (bp === null) return <Badge variant="muted">{t('analytics:deltaNew')}</Badge>
  if (Math.abs(bp) < 50) return <Badge variant="muted">{t('analytics:deltaFlat')}</Badge>
  const pct = (bp / 100).toLocaleString(locale, { maximumFractionDigits: 0 })
  return <Badge variant={bp > 0 ? 'success' : 'destructive'}>{bp > 0 ? `+${pct}%` : `${pct}%`}</Badge>
}

/// 拆分视图：高频 6 维度一键切换 + 头部构成占比带（与表格双向联动高亮）+ 行内下钻 / 追踪流向 / 查明细。
export function BreakdownView({ tabs, search }: { tabs?: React.ReactNode; search: AnalyticsSearch }) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const navigate = useNavigate({ from: '/admin/stats' })
  const [hoveredKey, setHoveredKey] = useState<string | null>(null)
  const by = effectiveBy(search)
  const params = cubeParams(search, { by, limit: '50', metric: search.metric === 'amount' ? undefined : search.metric })
  const q = useQuery({
    queryKey: qk.statsBreakdown(params),
    queryFn: () => apiFetch<BreakdownResp>(`/admin/stats/breakdown?${params}`),
    retry: false,
  })

  const dimLabel = Object.fromEntries(BREAKDOWN_DIMS.map((d) => [d, dimensionLabel(t, d)]))

  const focusPatch = (row: BreakdownRow): Partial<AnalyticsSearch> | null => {
    if (!row.key) return null
    switch (by) {
      case 'model':
        return { model: row.key }
      case 'channel':
        return { channel_id: row.channel_id }
      case 'user':
        return { user_id: row.user_id ?? undefined }
      case 'api_key':
        return { api_key_id: row.api_key_id }
      case 'group':
        return { group: row.key }
      case 'requested_model':
        return { model_source: 'requested', model: row.key }
      case 'upstream_model':
        return { model_source: 'upstream', model: row.key }
      case 'endpoint':
      case 'upstream_endpoint':
      case 'node':
      case 'request_type':
      case 'billing_type':
        return { [by]: row.key }
      default:
        return null
    }
  }

  const focus = (row: BreakdownRow) => {
    const patch = focusPatch(row)
    if (!patch) return
    // 聚焦后旧的拆分维度已成单行，交给 effectiveBy 换下一层
    void navigate({ search: (prev) => cleanSearch({ ...prev, ...patch, by: undefined }) })
  }

  const traceInFlow = (row: BreakdownRow) => {
    const patch = focusPatch(row)
    if (!patch) return
    void navigate({ search: (prev) => cleanSearch({ ...prev, ...patch, view: 'flow', by: undefined }) })
  }

  const logsSearch = (row: BreakdownRow): LogSearch => {
    const s: LogSearch = {
      model: search.model,
      user_id: search.user_id,
      api_key_id: search.api_key_id,
      channel_id: search.channel_id,
      // 超过日志页 90 天上限的窗口改用绝对起点：hours 会被后端静默钳到 2160，对账漏掉更早的记录
      ...logsWindow(effectiveDays(search)),
    }
    if (by === 'model') s.model = row.key
    if (by === 'channel') s.channel_id = row.channel_id
    if (by === 'user') s.user_id = row.user_id ?? undefined
    if (by === 'api_key') s.api_key_id = row.api_key_id
    return s
  }

  const secondary = (row: BreakdownRow): string | null => {
    switch (by) {
      case 'channel':
        return row.provider ?? null
      case 'api_key':
        return [row.key_prefix ? `${row.key_prefix}…` : null, row.username ?? null]
          .filter((x): x is string => x !== null)
          .join(' · ') || null
      case 'group':
        return row.group_ratio ? `×${formatRatio(row.group_ratio)}` : null
      case 'provider':
        return t('analytics:providerChannels', { n: row.channels ?? 0 })
      case 'user':
        return `#${row.key}`
      default:
        return null
    }
  }

  const displayNameOf = (row: BreakdownRow) =>
    by === 'channel'
      ? maskEmails(row.label || row.key || t('analysis:notCollected'))
      : row.label || row.key || t('analysis:notCollected')

  const rows = q.data?.data ?? []
  const sortMetric: FlowMetric = search.metric ?? 'amount'
  const setSortMetric = (metric: FlowMetric) =>
    void navigate({ resetScroll: false, search: (prev) => cleanSearch({ ...prev, metric }) })
  const setBy = (nextBy: BreakdownDim) =>
    void navigate({ search: (prev) => cleanSearch({ ...prev, by: nextBy }) })

  const shareOf = (row: BreakdownRow): number =>
    sortMetric === 'requests'
      ? (row.request_share_bp ?? row.share_bp)
      : sortMetric === 'tokens'
        ? (row.token_share_bp ?? row.share_bp)
        : row.share_bp
  const maxShare = rows.reduce((m, r) => Math.max(m, shareOf(r)), 0)
  const monoEntity = ['model', 'requested_model', 'upstream_model', 'endpoint', 'upstream_endpoint', 'node'].includes(by)
  // 毛利列只在窗口内有成本数据时出现：成本采集上线前的历史行全是 0，摆一列 100% 毛利只会误导
  const hasCost = rows.some((r) => (r.cost_known_requests ?? 0) > 0)
  const isPrimaryDim = PRIMARY_DIMS.includes(by)

  // 头部构成占比带：取当前排序口径下占比为正的前 5 项，余项折入「其他」，帮助一眼识别集中度风险。
  const topSegments = rows
    .filter((r) => shareOf(r) > 0)
    .slice(0, 5)
    .map((row, idx) => ({
      key: row.key,
      row,
      label: displayNameOf(row),
      bp: shareOf(row),
      color: chartColor(idx),
      focusable: by !== 'provider' && !!row.key,
    }))
  const topSumBp = topSegments.reduce((sum, seg) => sum + seg.bp, 0)
  const showComposition = !q.isError && !q.isLoading && topSegments.length >= 2 && topSumBp > 0 && topSumBp <= 10_500
  const otherBp = showComposition && rows.length > topSegments.length ? Math.max(0, 10_000 - topSumBp) : 0
  const topColorByKey = new Map(topSegments.map((seg) => [seg.key, seg.color]))

  const sortHeader = (metric: FlowMetric, label: string, align: 'left' | 'right' = 'right') => {
    const active = sortMetric === metric
    return (
      <button
        type="button"
        onClick={() => setSortMetric(metric)}
        className={cn(
          'inline-flex cursor-pointer items-center gap-1 rounded-sm outline-none transition-colors hover:text-foreground focus-visible:ring-2 focus-visible:ring-primary/40',
          align === 'right' && 'ml-auto flex-row-reverse',
          active && 'font-semibold text-primary',
        )}
      >
        <span>{label}</span>
        {active && <ArrowDown aria-hidden className="h-3 w-3 shrink-0" />}
      </button>
    )
  }

  const headerSummary = q.data ? (
    <>
      <span>
        {t('analytics:breakdownTotal', {
          amount: formatMoneyAggregate(q.data.total_amount_micro, locale),
          n: formatCount(q.data.total_requests, locale),
        })}
      </span>
      {rows.length > 0 && (
        <>
          <span aria-hidden className="text-border">·</span>
          <span>{t('analytics:breakdownItems', { n: rows.length })}</span>
        </>
      )}
    </>
  ) : undefined

  return (
    <Card className="min-w-0 overflow-hidden">
      <WorkspaceHeader tabs={tabs} summary={headerSummary} />

      <div className="flex flex-wrap items-center justify-between gap-3 border-b border-border/55 px-4 py-2.5 sm:px-5">
        <div className="flex flex-wrap items-center gap-1.5">
          <div className="inline-flex flex-wrap items-center gap-0.5 rounded-lg border border-border/80 bg-muted/55 p-0.5">
            {PRIMARY_DIMS.map((d) => {
              const active = by === d
              const disabled = dimFiltered(d, search)
              return (
                <button
                  key={d}
                  type="button"
                  disabled={disabled}
                  onClick={() => setBy(d)}
                  className={cn(
                    'h-7 rounded-md px-2.5 text-xs transition-colors outline-none focus-visible:ring-2 focus-visible:ring-primary/40',
                    disabled
                      ? 'cursor-not-allowed text-muted-foreground/45 line-through decoration-muted-foreground/30'
                      : active
                        ? 'bg-card font-medium text-foreground shadow-xs ring-1 ring-inset ring-border/55 dark:ring-white/[0.08]'
                        : 'cursor-pointer text-muted-foreground hover:text-foreground',
                  )}
                >
                  {dimLabel[d]}
                </button>
              )
            })}
          </div>
          {/* 保留完整 12 维度的原生 select（与自动化及键盘导航兼容）：选主维度时收成「更多维度 ▾」避免文案重复 */}
          <div
            className={cn(
              'relative inline-flex h-8 items-center gap-1 rounded-lg border px-2.5 text-xs transition-colors has-[:focus-visible]:ring-2 has-[:focus-visible]:ring-primary/40',
              isPrimaryDim
                ? 'border-border/80 bg-card text-muted-foreground hover:border-muted-foreground/40 hover:text-foreground'
                : 'border-primary/40 bg-primary/8 font-medium text-primary',
            )}
          >
            <span aria-hidden className="pointer-events-none whitespace-nowrap">
              {isPrimaryDim ? t('analytics:moreDims') : dimLabel[by]}
            </span>
            <ChevronDown aria-hidden className="pointer-events-none h-3.5 w-3.5 opacity-70" />
            <select
              aria-label={t('analytics:breakdownBy')}
              className="absolute inset-0 h-full w-full cursor-pointer opacity-0"
              value={by}
              onChange={(e) => setBy(e.target.value as BreakdownDim)}
            >
              {BREAKDOWN_DIMS.map((d) => (
                <option key={d} value={d} disabled={dimFiltered(d, search)}>{dimLabel[d]}</option>
              ))}
            </select>
          </div>
        </div>

        <label className="inline-flex items-center gap-2 text-xs text-muted-foreground">
          <span className="whitespace-nowrap">{t('admin:dashboardSortBy')}</span>
          <select
            aria-label={t('admin:dashboardSortBy')}
            className={cn(compactSelectClass, 'min-w-28')}
            value={sortMetric}
            onChange={(e) => setSortMetric(e.target.value as FlowMetric)}
          >
            {FLOW_METRICS.map((value) => (
              <option key={value} value={value}>{t(`charts:metric_${value}`)}</option>
            ))}
          </select>
        </label>
      </div>

      {showComposition && (
        <div
          className="border-b border-border/55 bg-muted/15 px-4 py-3 sm:px-5"
          onMouseLeave={() => setHoveredKey(null)}
        >
          <div className="flex h-2 w-full gap-0.5 overflow-hidden rounded-full bg-muted/70">
            {topSegments.map((seg) => {
              const dimmed = hoveredKey !== null && hoveredKey !== seg.key
              const widthPct = Math.max(1.2, (seg.bp / Math.max(10_000, topSumBp)) * 100)
              return (
                <button
                  key={seg.key}
                  type="button"
                  disabled={!seg.focusable}
                  title={`${seg.label} · ${formatBp(seg.bp, locale)}`}
                  style={{ width: `${widthPct}%`, backgroundColor: seg.color }}
                  onMouseEnter={() => setHoveredKey(seg.key)}
                  onFocus={() => setHoveredKey(seg.key)}
                  onBlur={() => setHoveredKey(null)}
                  onClick={() => { if (seg.focusable) focus(seg.row) }}
                  className={cn(
                    'h-full rounded-[2px] transition-opacity duration-150 outline-none focus-visible:ring-2 focus-visible:ring-primary/50',
                    seg.focusable ? 'cursor-pointer' : 'cursor-default',
                    dimmed ? 'opacity-25' : 'opacity-90 hover:opacity-100',
                  )}
                />
              )
            })}
            {otherBp >= 10 && (
              <div
                title={`${t('analytics:other')} · ${formatBp(otherBp, locale)}`}
                style={{ width: `${(otherBp / Math.max(10_000, topSumBp)) * 100}%`, backgroundColor: OTHER_COLOR }}
                className={cn('h-full rounded-[2px] transition-opacity duration-150', hoveredKey !== null ? 'opacity-20' : 'opacity-50')}
              />
            )}
          </div>
          <div className="mt-2 flex flex-wrap items-center gap-x-4 gap-y-1 text-[11.5px]">
            <span className="font-medium text-muted-foreground/80">{t('analytics:topShareTitle')}</span>
            {topSegments.map((seg) => {
              const active = hoveredKey === seg.key
              const dimmed = hoveredKey !== null && !active
              return (
                <button
                  key={seg.key}
                  type="button"
                  disabled={!seg.focusable}
                  onMouseEnter={() => setHoveredKey(seg.key)}
                  onClick={() => { if (seg.focusable) focus(seg.row) }}
                  className={cn(
                    'inline-flex max-w-52 items-center gap-1.5 rounded-sm text-left transition-opacity duration-150 outline-none focus-visible:ring-2 focus-visible:ring-primary/40',
                    seg.focusable ? 'cursor-pointer hover:text-primary' : 'cursor-default',
                    dimmed && 'opacity-40',
                    active && 'text-foreground',
                  )}
                >
                  <span aria-hidden className="h-2 w-2 shrink-0 rounded-full" style={{ backgroundColor: seg.color }} />
                  <span className={cn('truncate text-foreground/90', monoEntity && 'font-mono text-[11px]', active && 'font-medium')}>
                    {seg.label}
                  </span>
                  <span className="shrink-0 text-muted-foreground tabular-nums">{formatBp(seg.bp, locale)}</span>
                </button>
              )
            })}
            {otherBp >= 10 && (
              <span className={cn('inline-flex items-center gap-1.5 text-muted-foreground transition-opacity duration-150', hoveredKey !== null && 'opacity-40')}>
                <span aria-hidden className="h-2 w-2 shrink-0 rounded-full opacity-60" style={{ backgroundColor: OTHER_COLOR }} />
                <span>{t('analytics:other')}</span>
                <span className="tabular-nums">{formatBp(otherBp, locale)}</span>
              </span>
            )}
          </div>
        </div>
      )}

      {q.isError ? (
        <div className="p-4 sm:p-5">
          <ErrorState message={describeError(q.error)} />
        </div>
      ) : q.isLoading ? (
        <div className="p-4 sm:p-5">
          <LoadingState />
        </div>
      ) : rows.length === 0 ? (
        <div className="p-4 sm:p-5">
          <EmptyState hint={t('admin:trendEmptyHint')} />
        </div>
      ) : (
        <Table wrapperClassName="rounded-none border-0 shadow-none">
          <THead>
            <Tr>
              <Th className="w-14 pl-4 sm:pl-5">#</Th>
              <Th>{dimLabel[by]}</Th>
              <Th className={cn(sortMetric === 'amount' && 'min-w-44')}>{sortHeader('amount', t('analytics:kpiSpend'), 'left')}</Th>
              <Th>{t('analytics:colDelta')}</Th>
              {hasCost && <Th numeric>{t('analysis:coveredMargin')}</Th>}
              <Th numeric className={cn(sortMetric === 'requests' && 'min-w-36')}>{sortHeader('requests', t('common:requests'))}</Th>
              <Th numeric className={cn(sortMetric === 'tokens' && 'min-w-36')}>{sortHeader('tokens', t('common:tokens'))}</Th>
              <Th numeric>{t('analytics:kpiLatency')}</Th>
              <Th className="w-28 pr-4 text-right sm:pr-5">{t('common:actions')}</Th>
            </Tr>
          </THead>
          <TBody>
            {rows.map((row) => {
              const displayName = displayNameOf(row)
              const focusable = by !== 'provider' && !!row.key
              const canTraceFlow = focusable && FLOW_TRACE_DIMS.has(by)
              const canViewLogs = ['model', 'channel', 'user', 'api_key'].includes(by) && !ADVANCED_KEYS.some((k) => search[k] !== undefined)
              const providerKey = by === 'provider' ? row.key : by === 'channel' ? row.provider : undefined
              const barPct = maxShare > 0 ? (shareOf(row) / maxShare) * 100 : 0
              const segColor = showComposition ? topColorByKey.get(row.key) : undefined
              const highlighted = hoveredKey !== null && hoveredKey === row.key
              return (
                <Tr
                  key={row.key}
                  onMouseEnter={() => { if (segColor) setHoveredKey(row.key) }}
                  onMouseLeave={() => { if (hoveredKey === row.key) setHoveredKey(null) }}
                  className={cn(highlighted && 'bg-primary/[0.05] dark:bg-primary/[0.09]')}
                >
                  <Td className="pl-4 sm:pl-5">
                    <div className="flex flex-col items-start gap-0.5 leading-tight">
                      <span
                        className={cn(
                          'inline-flex h-5 min-w-5 items-center justify-center rounded-md px-1 text-xs tabular-nums',
                          row.rank === 1
                            ? 'bg-primary/12 font-semibold text-primary ring-1 ring-inset ring-primary/15'
                            : row.rank <= 3
                              ? 'bg-muted/75 font-semibold text-foreground/85 ring-1 ring-inset ring-border/75'
                              : 'text-muted-foreground',
                        )}
                      >
                        {row.rank}
                      </span>
                      <span className="pl-0.5">
                        <RankChange row={row} />
                      </span>
                    </div>
                  </Td>
                  <Td>
                    <div className="flex max-w-68 items-center gap-2.5">
                      {providerKey ? (
                        <ProviderMark provider={providerKey} size="sm" className="h-7 w-7 rounded-lg" />
                      ) : segColor ? (
                        <span aria-hidden className="h-2 w-2 shrink-0 rounded-full" style={{ backgroundColor: segColor }} />
                      ) : null}
                      <div className="flex min-w-0 flex-1 flex-col items-start leading-tight">
                        {focusable ? (
                          <button
                            type="button"
                            onClick={() => focus(row)}
                            className={cn(
                              'max-w-full cursor-pointer truncate rounded-sm text-left font-medium outline-none transition-colors hover:text-primary hover:underline hover:underline-offset-4 focus-visible:ring-2 focus-visible:ring-primary/40',
                              monoEntity && 'font-mono text-[12.5px]',
                            )}
                            title={by === 'channel' ? maskEmails(row.label ?? row.key) : row.label ?? row.key}
                          >
                            {displayName}
                          </button>
                        ) : (
                          <span
                            className={cn('max-w-full truncate font-medium', monoEntity && 'font-mono text-[12.5px]')}
                            title={by === 'channel' ? maskEmails(row.label ?? row.key) : row.label ?? row.key}
                          >
                            {displayName}
                          </span>
                        )}
                        {secondary(row) !== null && (
                          <span className="mt-0.5 max-w-full truncate text-[11px] text-muted-foreground">{secondary(row)}</span>
                        )}
                      </div>
                    </div>
                  </Td>
                  <Td>
                    {sortMetric === 'amount' ? (
                      <div className="flex flex-col gap-1.5">
                        <div className="flex items-baseline justify-between gap-2">
                          {/* 行金额保留四位：单个模型/密钥一周可能只花 $0.0034，两位小数会显示成 $0.00 像免费 */}
                          <span className="font-semibold text-foreground tabular-nums">
                            {formatMoney(row.amount_micro, locale)}
                          </span>
                          <span className="text-[11px] text-muted-foreground tabular-nums">
                            {formatBp(row.share_bp, locale)}
                          </span>
                        </div>
                        <div className="h-1.5 w-full overflow-hidden rounded-full bg-muted/85">
                          <div
                            className="h-full rounded-full bg-primary/80 transition-[width] duration-300"
                            style={{ width: `${barPct}%`, backgroundColor: segColor }}
                          />
                        </div>
                      </div>
                    ) : (
                      <div className="flex flex-col items-start leading-tight">
                        <span className="font-medium tabular-nums">
                          {formatMoney(row.amount_micro, locale)}
                        </span>
                        <span className="mt-0.5 text-[11px] text-muted-foreground tabular-nums">
                          {formatBp(row.share_bp, locale)}
                        </span>
                      </div>
                    )}
                  </Td>
                  <Td>
                    <DeltaBadge bp={row.delta_bp} locale={locale} />
                  </Td>
                  {hasCost && <MarginCell row={row} locale={locale} />}
                  <Td numeric>
                    <div className="flex flex-col items-end gap-1 leading-tight">
                      <div className="flex items-baseline gap-1.5">
                        <span className={cn('tabular-nums', sortMetric === 'requests' ? 'font-semibold text-foreground' : 'font-medium')}>
                          {formatCount(row.requests, locale)}
                        </span>
                        {sortMetric === 'requests' && row.request_share_bp != null && (
                          <span className="text-[11px] text-muted-foreground tabular-nums">
                            {formatBp(row.request_share_bp, locale)}
                          </span>
                        )}
                      </div>
                      {sortMetric === 'requests' && (
                        <div className="h-1.5 w-24 overflow-hidden rounded-full bg-muted/85">
                          <div
                            className="ml-auto h-full rounded-full bg-primary/80 transition-[width] duration-300"
                            style={{ width: `${barPct}%`, backgroundColor: segColor }}
                          />
                        </div>
                      )}
                      <span
                        className={cn(
                          'text-[11px] tabular-nums',
                          row.error_rate_bp >= BAD_BP
                            ? 'font-medium text-destructive'
                            : row.error_rate_bp >= WARN_BP
                              ? 'font-medium text-warning'
                              : 'text-muted-foreground',
                        )}
                      >
                        {t('analytics:errRate', { v: row.requests > 0 ? formatBp(row.error_rate_bp, locale) : '—' })}
                      </span>
                    </div>
                  </Td>
                  <Td numeric>
                    <div className="flex flex-col items-end gap-1 leading-tight">
                      <div className="flex items-baseline gap-1.5">
                        <span className={cn('tabular-nums', sortMetric === 'tokens' ? 'font-semibold text-foreground' : 'font-medium')}>
                          {formatCount(row.tokens, locale)}
                        </span>
                        {sortMetric === 'tokens' && row.token_share_bp != null && (
                          <span className="text-[11px] text-muted-foreground tabular-nums">
                            {formatBp(row.token_share_bp, locale)}
                          </span>
                        )}
                      </div>
                      {sortMetric === 'tokens' && (
                        <div className="h-1.5 w-24 overflow-hidden rounded-full bg-muted/85">
                          <div
                            className="ml-auto h-full rounded-full bg-primary/80 transition-[width] duration-300"
                            style={{ width: `${barPct}%`, backgroundColor: segColor }}
                          />
                        </div>
                      )}
                      <span className="text-[11px] text-muted-foreground tabular-nums">
                        {t('analytics:cacheRate', { v: row.cache_hit_bp == null ? '—' : formatBp(row.cache_hit_bp, locale) })}
                      </span>
                    </div>
                  </Td>
                  <Td numeric>
                    {row.requests > 0 && row.avg_latency_ms != null ? (
                      <div className="flex flex-col items-end leading-tight">
                        <span className="font-medium tabular-nums">{formatCount(row.avg_latency_ms, locale)} ms</span>
                        {row.avg_ttft_ms != null && (row.ttft_samples ?? row.avg_ttft_ms) > 0 && (
                          <span className="mt-0.5 text-[11px] text-muted-foreground tabular-nums">
                            {t('analytics:ttftShort', { v: formatCount(row.avg_ttft_ms, locale) })}
                          </span>
                        )}
                      </div>
                    ) : (
                      <span className="text-muted-foreground">—</span>
                    )}
                  </Td>
                  <Td className="pr-4 sm:pr-5">
                    {(focusable || canViewLogs) && (
                      <div className="flex items-center justify-end">
                        <ActionGroup label={t('common:actions')}>
                          {focusable && (
                            <IconButton
                              icon={Crosshair}
                              label={t('analytics:focus')}
                              onClick={() => focus(row)}
                            />
                          )}
                          {canTraceFlow && (
                            <IconButton
                              icon={GitFork}
                              label={t('analytics:viewInFlow')}
                              onClick={() => traceInFlow(row)}
                            />
                          )}
                          {canViewLogs && (
                            <IconButton
                              icon={ScrollText}
                              label={t('analytics:viewLogs')}
                              onClick={() => void navigate({ to: '/admin/logs', search: logsSearch(row) })}
                            />
                          )}
                        </ActionGroup>
                      </div>
                    )}
                  </Td>
                </Tr>
              )
            })}
          </TBody>
        </Table>
      )}
      <div className="border-t border-border/60 bg-muted/20 px-4 py-2.5 text-xs text-muted-foreground sm:px-5">
        {t('analysis:unknownHint')} {hasCost && t('analysis:costHint')}
      </div>
    </Card>
  )
}
