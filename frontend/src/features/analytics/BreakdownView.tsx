import { useQuery } from '@tanstack/react-query'
import { useNavigate } from '@tanstack/react-router'
import { Crosshair, ScrollText } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import type { AnalyticsSearch, BreakdownDim } from '@/routes/admin.stats'
import { BREAKDOWN_DIMS } from '@/routes/admin.stats'
import type { LogSearch } from '@/routes/admin.logs'
import { Badge } from '@/components/ui/badge'
import { Card, CardContent } from '@/components/ui/card'
import { IconButton } from '@/components/ui/icon-button'
import { dimensionLabel, ADVANCED_KEYS, selectClass } from './AnalysisControls'
import { EmptyState, ErrorState, LoadingState } from '@/components/ui/state'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { cleanSearch, cubeParams, effectiveDays } from '@/features/analytics/search'
import type { BreakdownResp, BreakdownRow } from '@/features/analytics/types'
import { BAD_BP, WARN_BP } from '@/features/stats/types'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatBp, formatCount, formatMoney, formatMoneyAggregate, formatRatio } from '@/lib/money'
import { qk } from '@/lib/query-keys'
import { cn } from '@/lib/utils'

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

/// 名次变化：▲2 升了两位、▼1 降了一位、"新"上期不在榜、"—"持平。
/// 毛利 = 实收 − 上游成本（官方价 × 渠道相对成本系数）；负毛利标红——
/// 折扣组打在贵渠道上就会亏，这是渠道维度拆解最该回答的问题。
function MarginCell({ row, locale }: { row: BreakdownRow; locale: string }) {
  const { t } = useTranslation()
  const margin = row.known_margin_micro
  return <Td><div className="flex flex-col gap-1"><span className={cn('tabular-nums', margin != null && margin < 0 && 'text-destructive')}>{margin == null ? '—' : formatMoney(margin, locale)}</span><span className="text-xs text-muted-foreground">{t('analysis:coverage', { v: row.cost_coverage_bp == null ? '—' : formatBp(row.cost_coverage_bp, locale) })}</span></div></Td>
}

function RankChange({ row }: { row: BreakdownRow }) {
  const { t } = useTranslation()
  if (row.previous_rank === null) {
    return <span className="text-[10px] text-muted-foreground">{t('analytics:deltaNew')}</span>
  }
  const d = row.previous_rank - row.rank
  if (d === 0) return <span className="text-[10px] text-muted-foreground">—</span>
  return (
    <span className={cn('text-[10px] tabular-nums', d > 0 ? 'text-success' : 'text-destructive')}>
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

/// 拆分视图：过滤后按另一维度看构成（Sub2API UserBreakdown / new-api 排行榜的合体）。
///
/// 每行是一个可下钻的入口：点"聚焦"把这一行变成过滤条件，页面随即回答下一层
/// 问题（"gpt-4o 花了六成"→ 聚焦 →"它走了哪些渠道 / 谁在用"）；点"日志"直达
/// 这一行对应的明细。下钻路径全在 URL，浏览器后退就是上一层。
export function BreakdownView({ search }: { search: AnalyticsSearch }) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const navigate = useNavigate({ from: '/admin/stats' })
  const by = effectiveBy(search)
  const params = cubeParams(search, { by, limit: '50' })
  const q = useQuery({
    queryKey: qk.statsBreakdown(params),
    queryFn: () => apiFetch<BreakdownResp>(`/admin/stats/breakdown?${params}`),
    retry: false,
  })

  const dimLabel = Object.fromEntries(BREAKDOWN_DIMS.map((d) => [d, dimensionLabel(t, d)]))

  const focus = (row: BreakdownRow) => {
    if (!row.key) return
    const patch: Partial<AnalyticsSearch> = {}
    switch (by) {
      case 'model':
        patch.model = row.key
        break
      case 'channel':
        patch.channel_id = row.channel_id
        break
      case 'user':
        patch.user_id = row.user_id ?? undefined
        break
      case 'api_key':
        patch.api_key_id = row.api_key_id
        break
      case 'group':
        patch.group = row.key
        break
      case 'requested_model': patch.model_source = 'requested'; patch.model = row.key; break
      case 'upstream_model': patch.model_source = 'upstream'; patch.model = row.key; break
      case 'endpoint': case 'upstream_endpoint': case 'node': case 'request_type': case 'billing_type': patch[by] = row.key; break
      default: return
    }
    // 聚焦后旧的拆分维度已成单行，交给 effectiveBy 换下一层
    void navigate({ search: (prev) => cleanSearch({ ...prev, ...patch, by: undefined }) })
  }

  const logsSearch = (row: BreakdownRow): LogSearch => {
    const s: LogSearch = {
      model: search.model,
      user_id: search.user_id,
      api_key_id: search.api_key_id,
      channel_id: search.channel_id,
      hours: effectiveDays(search) * 24,
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

  const rows = q.data?.data ?? []
  const maxShare = rows.reduce((m, r) => Math.max(m, r.share_bp), 0)
  // 毛利列只在窗口内有成本数据时出现：成本采集上线前的历史行全是 0，
  // 摆一列 100% 毛利只会误导
  const hasCost = rows.some((r) => (r.cost_known_requests ?? 0) > 0)

  return (
    <Card>
      <CardContent className="flex flex-col gap-3 pt-4">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <div className="flex items-center gap-2">
            <span className="text-xs text-muted-foreground">{t('analytics:breakdownBy')}</span>
            <select aria-label={t('analytics:breakdownBy')} className={selectClass} value={by} onChange={(e) => void navigate({ search: (prev) => cleanSearch({ ...prev, by: e.target.value as BreakdownDim }) })}>{BREAKDOWN_DIMS.map((d) => <option key={d} value={d} disabled={dimFiltered(d, search)}>{dimLabel[d]}</option>)}</select>
          </div>
          {q.data && (
            <span className="text-xs text-muted-foreground">
              {t('analytics:breakdownTotal', {
                amount: formatMoneyAggregate(q.data.total_amount_micro, locale),
                n: formatCount(q.data.total_requests, locale),
              })}
            </span>
          )}
        </div>

        {q.isError ? (
          <ErrorState message={describeError(q.error)} />
        ) : q.isLoading ? (
          <LoadingState />
        ) : rows.length === 0 ? (
          <EmptyState hint={t('admin:trendEmptyHint')} />
        ) : (
          <Table>
            <THead>
              <Tr>
                <Th className="w-12">#</Th>
                <Th>{dimLabel[by]}</Th>
                <Th className="min-w-48">{t('analytics:kpiSpend')}</Th>
                <Th>{t('analytics:colDelta')}</Th>
                {hasCost && <Th>{t('analysis:coveredMargin')}</Th>}
                <Th>{t('common:requests')}</Th>
                <Th>{t('common:tokens')}</Th>
                <Th>{t('analytics:kpiLatency')}</Th>
                <Th className="w-20">{t('common:actions')}</Th>
              </Tr>
            </THead>
            <TBody>
              {rows.map((row) => (
                <Tr key={row.key}>
                  <Td>
                    <div className="flex flex-col leading-tight">
                      <span className="tabular-nums">{row.rank}</span>
                      <RankChange row={row} />
                    </div>
                  </Td>
                  <Td>
                    <div className="flex max-w-64 flex-col leading-tight">
                      <span className="truncate font-medium" title={row.label ?? row.key}>
                        {row.label || row.key || t('analysis:notCollected')}
                      </span>
                      {secondary(row) !== null && (
                        <span className="truncate text-xs text-muted-foreground">{secondary(row)}</span>
                      )}
                    </div>
                  </Td>
                  <Td>
                    <div className="flex flex-col gap-1">
                      <div className="flex items-baseline justify-between gap-2">
                        {/* 行金额保留四位：单个模型/密钥一周可能只花 $0.0034，两位小数会显示成 $0.00 像免费 */}
                        <span className="tabular-nums">{formatMoney(row.amount_micro, locale)}</span>
                        <span className="text-xs text-muted-foreground tabular-nums">
                          {formatBp(row.share_bp, locale)}
                        </span>
                      </div>
                      {/* 占比条按最大值归一：让第一名满格，其余相对可比 */}
                      <div className="h-1 w-full overflow-hidden rounded-full bg-muted">
                        <div
                          className="h-full rounded-full bg-primary/70"
                          style={{ width: `${maxShare > 0 ? (row.share_bp / maxShare) * 100 : 0}%` }}
                        />
                      </div>
                    </div>
                  </Td>
                  <Td>
                    <DeltaBadge bp={row.delta_bp} locale={locale} />
                  </Td>
                  {hasCost && <MarginCell row={row} locale={locale} />}
                  <Td>
                    <div className="flex flex-col leading-tight">
                      <span className="tabular-nums">{formatCount(row.requests, locale)}</span>
                      <span
                        className={cn(
                          'text-xs tabular-nums',
                          row.error_rate_bp >= BAD_BP
                            ? 'text-destructive'
                            : row.error_rate_bp >= WARN_BP
                              ? 'text-warning'
                              : 'text-muted-foreground',
                        )}
                      >
                        {t('analytics:errRate', { v: formatBp(row.error_rate_bp, locale) })}
                      </span>
                    </div>
                  </Td>
                  <Td>
                    <div className="flex flex-col leading-tight">
                      <span className="tabular-nums">{formatCount(row.tokens, locale)}</span>
                      <span className="text-xs text-muted-foreground tabular-nums">
                        {t('analytics:cacheRate', { v: formatBp(row.cache_hit_bp, locale) })}
                      </span>
                    </div>
                  </Td>
                  <Td>
                    {row.avg_latency_ms > 0 ? (
                      <div className="flex flex-col leading-tight">
                        <span className="tabular-nums">{formatCount(row.avg_latency_ms, locale)} ms</span>
                        {row.avg_ttft_ms > 0 && (
                          <span className="text-xs text-muted-foreground tabular-nums">
                            {t('analytics:ttftShort', { v: formatCount(row.avg_ttft_ms, locale) })}
                          </span>
                        )}
                      </div>
                    ) : (
                      <span className="text-muted-foreground">—</span>
                    )}
                  </Td>
                  <Td>
                    <div className="flex gap-1">
                      {by !== 'provider' && !!row.key && (
                        <IconButton
                          icon={Crosshair}
                          label={t('analytics:focus')}
                          onClick={() => focus(row)}
                        />
                      )}
                      {['model', 'channel', 'user', 'api_key'].includes(by) && !ADVANCED_KEYS.some((k) => search[k] !== undefined) && (
                        <IconButton
                          icon={ScrollText}
                          label={t('analytics:viewLogs')}
                          onClick={() => void navigate({ to: '/admin/logs', search: logsSearch(row) })}
                        />
                      )}
                    </div>
                  </Td>
                </Tr>
              ))}
            </TBody>
          </Table>
        )}
        <p className="text-xs text-muted-foreground">{t('analysis:unknownHint')} {hasCost && t('analysis:costHint')}</p>
      </CardContent>
    </Card>
  )
}
