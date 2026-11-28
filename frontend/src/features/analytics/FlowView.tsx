import { useQuery } from '@tanstack/react-query'
import { useNavigate } from '@tanstack/react-router'
import { Check, ChevronRight } from 'lucide-react'
import { useMemo, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Layer, Rectangle, ResponsiveContainer, Sankey, Tooltip } from 'recharts'
import type { AnalyticsSearch, FlowMetric } from '@/features/analytics/route-state'
import { FLOW_METRICS } from '@/features/analytics/route-state'
import { Badge } from '@/components/ui/badge'
import { Card } from '@/components/ui/card'
import { Segmented } from '@/components/ui/segmented'
import { Select } from '@/components/ui/select'
import { EmptyState, ErrorState, LoadingState } from '@/components/ui/state'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { flowIdentity, flowShortName } from './flow-labels'
import { WorkspaceHeader } from './WorkspaceHeader'
import { cleanSearch } from '@/features/analytics/search'
import { cubeParams } from '@/lib/cube'
import type { FlowNode, FlowResp } from '@/lib/cube'
import { apiFetch } from '@/lib/api'
import { OTHER_COLOR, chartColor } from '@/lib/chart'
import { describeError } from '@/lib/i18n'
import { formatBp, formatCount, formatMoneyAggregate } from '@/lib/money'
import { qk } from '@/lib/query-keys'
import { cn } from '@/lib/utils'

const STAGE_ORDER: FlowNode['stage'][] = ['user', 'node', 'api_key', 'group', 'model', 'channel']

/// 阶段配色：同一阶段同色，跨阶段用分类色板的相邻色——读者按列识别阶段，
/// 而不是按节点识别实体（实体名写在节点旁）。
function stageColor(stage: FlowNode['stage']): string {
  return chartColor(STAGE_ORDER.indexOf(stage))
}

/// 流向视图：用户 → 网关节点 → 密钥 → 分组 → 模型 → 渠道 的桑基图。
///
/// 悬停任意节点或路径时，仅保留直接关联的上下游节点与连线清晰高亮，
/// 其余路径柔化退后；悬浮卡展示阶段、占比与下钻提示，点击具名节点直接设为筛选条件。
export function FlowView({ tabs, search }: { tabs?: React.ReactNode; search: AnalyticsSearch }) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const navigate = useNavigate({ from: '/admin/stats' })
  const [hoveredNode, setHoveredNode] = useState<number | null>(null)
  const [hoveredLink, setHoveredLink] = useState<number | null>(null)
  const [detailStage, setDetailStage] = useState<FlowNode['stage'] | 'all'>('all')
  const metric: FlowMetric = search.metric ?? 'amount'
  const configuredStages = STAGE_ORDER.filter((s) => !search.stages || search.stages.includes(s))
  const params = cubeParams(search, { metric, limit: String(search.limit ?? 6), stages: search.stages ? JSON.stringify(search.stages) : undefined })
  const q = useQuery({
    queryKey: qk.statsFlow(params),
    queryFn: () => apiFetch<FlowResp>(`/admin/stats/flow?${params}`),
    retry: false,
  })

  const stages = q.data?.stages?.length ? STAGE_ORDER.filter((s) => q.data?.stages.includes(s)) : configuredStages

  const metricLabel: Record<FlowMetric, string> = {
    amount: t('analytics:kpiSpend'),
    requests: t('common:requests'),
    tokens: t('common:tokens'),
  }
  const stageLabel: Record<FlowNode['stage'], string> = {
    user: t('analytics:dimUser'),
    node: t('analysis:node'),
    api_key: t('analytics:dimApiKey'),
    group: t('analytics:dimGroup'),
    model: t('analytics:dimModel'),
    channel: t('analytics:dimChannel'),
  }
  const fmt = (v: number) =>
    metric === 'amount' ? formatMoneyAggregate(v, locale, true) : formatCount(v, locale)

  const detailNodes = q.isError ? [] : q.data?.nodes ?? []
  const hasNegativeFlows = !q.isError && !!q.data && (q.data.nodes.some((n) => n.value < 0) || q.data.links.some((l) => l.value < 0))

  // Recharts 的 Sankey 按数组下标引用节点：把 id → 下标映射一次
  const graph = useMemo(() => {
    const resp = q.data
    // Signed refunds must remain in the details. A positive-only Sankey would
    // overstate net spending and break conservation at the connecting stages.
    if (!resp || q.isError || hasNegativeFlows) return null
    const nodes = resp.nodes.filter((n) => n.value > 0)
    const index = new Map(nodes.map((n, i) => [n.id, i]))
    const links = resp.links
      .filter((l) => index.has(l.source) && index.has(l.target) && l.value > 0)
      .map((l) => ({ source: index.get(l.source) ?? 0, target: index.get(l.target) ?? 0, value: l.value }))
    return { nodes, links }
  }, [q.data, q.isError, hasNegativeFlows])

  // 计算悬停焦点邻域（节点 + 直接相连的上下游节点），让非关联节点同步柔化。
  const connectedNodes = useMemo(() => {
    if (!graph) return null
    if (hoveredLink !== null) {
      const edge = graph.links[hoveredLink]
      if (!edge) return null
      return new Set([edge.source, edge.target])
    }
    if (hoveredNode !== null) {
      const set = new Set<number>([hoveredNode])
      for (const edge of graph.links) {
        if (edge.source === hoveredNode) set.add(edge.target)
        if (edge.target === hoveredNode) set.add(edge.source)
      }
      return set
    }
    return null
  }, [graph, hoveredLink, hoveredNode])

  const nodeName = (n: FlowNode) => flowIdentity(n, t).primary
  const nodeClickable = (n: FlowNode) => !n.other && !!n.key && !(['user', 'api_key', 'channel'].includes(n.stage) && n.key === '0')
  const nodeDetail = (n: FlowNode) => {
    const identity = flowIdentity(n, t)
    return [stageLabel[n.stage], identity.primary, identity.detail, fmt(n.value), q.data?.total ? formatBp(Math.round(n.value / q.data.total * 10_000), locale) : ''].filter(Boolean).join(' · ')
  }
  const actualStages = STAGE_ORDER.filter((stage) => detailNodes.some((n) => n.stage === stage))
  const effectiveDetailStage = detailStage === 'all' || actualStages.includes(detailStage) ? detailStage : 'all'
  const visibleDetailStages = effectiveDetailStage === 'all' ? actualStages : [effectiveDetailStage]
  const maxStageNodes = Math.max(1, ...actualStages.map((s) => graph?.nodes.filter((n) => n.stage === s).length ?? 0))
  const graphHeight = Math.max(420, maxStageNodes * 42 + 48)
  const missingNames = detailNodes.some((n) => flowIdentity(n, t).missing)
  const totalValue = q.data?.total ?? 0

  const focus = (n: FlowNode) => {
    if (!nodeClickable(n)) return
    const patch: Partial<AnalyticsSearch> = {}
    switch (n.stage) {
      case 'user':
        patch.user_id = Number(n.key)
        break
      case 'api_key':
        patch.api_key_id = Number(n.key)
        break
      case 'channel':
        patch.channel_id = Number(n.key)
        break
      case 'model':
        patch.model = n.key
        break
      case 'node':
        patch.node = n.key
        break
      case 'group':
        patch.group = n.key
        break
      default:
        return
    }
    void navigate({ search: (prev) => cleanSearch({ ...prev, ...patch }) })
  }

  const headerSummary = q.data && !q.isError ? (
    <>
      {!hasNegativeFlows && q.data.total > 0 && q.data.coverage_bp < 9_950 && (
        <Badge variant="warning" dot className="tabular-nums">
          {t('analytics:flowCoverage', { v: formatBp(q.data.coverage_bp, locale) })}
        </Badge>
      )}
      {detailNodes.length > 0 && (
        <span>{t('analytics:flowNodesCount', { n: detailNodes.length })}</span>
      )}
    </>
  ) : undefined

  return (
    <Card className="min-w-0 overflow-hidden">
      <WorkspaceHeader tabs={tabs} summary={headerSummary} />

      <div className="flex flex-wrap items-center gap-x-3 gap-y-2 border-b border-border/55 px-4 py-2.5 text-xs sm:px-5">
        <div className="flex items-center gap-2">
          <span className="text-muted-foreground">{t('analytics:flowMetric')}</span>
          <Segmented
            options={FLOW_METRICS.map((m) => ({ value: m, label: metricLabel[m] }))}
            value={metric}
            onChange={(v) => void navigate({ search: (prev) => cleanSearch({ ...prev, metric: v }) })}
            size="sm"
          />
        </div>

        <span aria-hidden className="hidden h-5 w-px bg-border/75 lg:block" />

        <div className="flex flex-wrap items-center gap-1.5">
          <span className="mr-0.5 text-muted-foreground">{t('analysis:stages')}</span>
          {STAGE_ORDER.map((stage) => {
            const checked = stages.includes(stage)
            const locked = checked && stages.length <= 2
            const color = stageColor(stage)
            return (
              <label
                key={stage}
                className={cn(
                  'inline-flex h-7 items-center gap-1.5 rounded-lg border px-2 transition-colors select-none',
                  locked ? 'cursor-not-allowed' : 'cursor-pointer',
                  checked
                    ? 'border-border/80 bg-card text-foreground shadow-xs'
                    : 'border-transparent text-muted-foreground hover:bg-muted/60 hover:text-foreground',
                )}
              >
                <span className="relative inline-flex h-3.5 w-3.5 shrink-0 items-center justify-center">
                  <input
                    type="checkbox"
                    checked={checked}
                    disabled={locked}
                    onChange={(e) =>
                      void navigate({
                        search: (prev) =>
                          cleanSearch({
                            ...prev,
                            stages: e.target.checked
                              ? STAGE_ORDER.filter((s) => stages.includes(s) || s === stage)
                              : stages.filter((s) => s !== stage),
                          }),
                      })
                    }
                    className="peer absolute inset-0 z-10 cursor-inherit appearance-none opacity-0"
                  />
                  {/* 复选框底色直接取该阶段色相，一枚控件同时传达「已启用 + 对应图列」 */}
                  <span
                    aria-hidden
                    style={checked ? { backgroundColor: color, borderColor: color } : undefined}
                    className={cn(
                      'flex h-3.5 w-3.5 items-center justify-center rounded-[4px] border transition-colors',
                      'border-muted-foreground/35 bg-card peer-focus-visible:ring-2 peer-focus-visible:ring-primary/40',
                      checked && 'text-white',
                      locked && 'opacity-70',
                    )}
                  >
                    {checked && <Check className="h-2.5 w-2.5" strokeWidth={3} />}
                  </span>
                </span>
                <span className="whitespace-nowrap">{stageLabel[stage]}</span>
              </label>
            )
          })}
        </div>

        <label className="ml-auto inline-flex items-center gap-2 text-muted-foreground">
          <span className="whitespace-nowrap">{t('analysis:top')}</span>
          <Select
            size="sm"
            aria-label={t('analysis:top')}
            className="min-w-16"
            value={String(search.limit ?? 6)}
            onChange={(v) => void navigate({ search: (prev) => cleanSearch({ ...prev, limit: Number(v) }) })}
            options={[3, 6, 10, 20].map((n) => ({ value: String(n), label: String(n) }))}
          />
        </label>
      </div>

      <div className="flex flex-col gap-4 p-4 sm:p-5">
        {q.isError ? (
          <ErrorState message={describeError(q.error)} />
        ) : q.isLoading ? (
          <LoadingState />
        ) : hasNegativeFlows ? (
          <p className="text-sm text-muted-foreground">{t('analytics:flowSignedHint')}</p>
        ) : graph === null || graph.nodes.length === 0 || graph.links.length === 0 ? (
          <EmptyState hint={t('admin:trendEmptyHint')} />
        ) : (
          <div className="max-w-full overflow-x-auto">
            <div style={{ height: graphHeight, minWidth: Math.max(620, actualStages.length * 145 + 40) }}>
              <ResponsiveContainer width="100%" height="100%">
                <Sankey
                  data={{
                    nodes: graph.nodes.map((n) => ({ name: [nodeName(n), flowIdentity(n, t).detail].filter(Boolean).join(' · ') })),
                    links: graph.links,
                  }}
                  nodeWidth={12}
                  nodePadding={28}
                  // 右侧留出两行标签的空间；标题由节点实际 x 坐标绘制，兼容不同阶段数。
                  margin={{ top: 44, right: 156, bottom: 24, left: 8 }}
                  // 链接按来源列着色，悬停节点或路径时高亮关联链路、柔化其余
                  link={(props) => {
                    const edge = graph.links[props.index]
                    const sourceNode = graph.nodes[edge?.source ?? -1]
                    const stroke = sourceNode && !sourceNode.other ? stageColor(sourceNode.stage) : OTHER_COLOR
                    const d = `M${props.sourceX},${props.sourceY} C${props.sourceControlX},${props.sourceY} ${props.targetControlX},${props.targetY} ${props.targetX},${props.targetY}`
                    const focused = hoveredLink !== null
                      ? hoveredLink === props.index
                      : hoveredNode !== null
                        ? edge?.source === hoveredNode || edge?.target === hoveredNode
                        : null
                    const opacity = focused === true ? 0.52 : focused === false ? 0.07 : 0.22
                    return (
                      <path
                        d={d}
                        fill="none"
                        stroke={stroke}
                        strokeOpacity={opacity}
                        strokeWidth={Math.max(1, props.linkWidth)}
                        className="transition-[stroke-opacity] duration-150"
                        onMouseEnter={() => setHoveredLink(props.index)}
                        onMouseLeave={() => setHoveredLink(null)}
                      />
                    )
                  }}
                  node={(props) => {
                    const n = graph.nodes[props.index]
                    if (!n) return <g />
                    const fill = n.other ? OTHER_COLOR : stageColor(n.stage)
                    const clickable = nodeClickable(n)
                    const identity = flowIdentity(n, t)
                    const firstInStage = graph.nodes.findIndex((node) => node.stage === n.stage) === props.index
                    const secondary = [identity.missing ? identity.id : '', fmt(n.value), identity.deleted ? t('flow:deleted') : ''].filter(Boolean).join(' · ')
                    const active = hoveredNode === props.index
                    const dimmed = connectedNodes !== null && !connectedNodes.has(props.index)
                    return (
                      <g key={n.id}>
                        {firstInStage && (
                          <g data-flow-stage={n.stage}>
                            <circle cx={props.x + 4} cy={18} r={3.5} fill={stageColor(n.stage)} />
                            <text x={props.x + 14} y={22} fontSize={11} fontWeight={500} className="fill-muted-foreground">
                              {stageLabel[n.stage]}
                            </text>
                          </g>
                        )}
                        <Layer
                          data-flow-node={n.id}
                          role={clickable ? 'button' : undefined}
                          tabIndex={clickable ? 0 : undefined}
                          aria-label={nodeDetail(n)}
                          style={{ opacity: dimmed ? 0.3 : 1 }}
                          className="transition-opacity duration-150"
                          onMouseEnter={() => setHoveredNode(props.index)}
                          onMouseLeave={() => setHoveredNode(null)}
                          onKeyDown={(e) => { if (clickable && (e.key === 'Enter' || e.key === ' ')) { e.preventDefault(); focus(n) } }}
                        >
                          <title>{nodeDetail(n)}</title>
                          <Rectangle
                            x={props.x}
                            y={props.y}
                            width={props.width}
                            height={props.height}
                            fill={fill}
                            fillOpacity={n.other ? 0.5 : active ? 1 : 0.92}
                            radius={3}
                            className={cn(clickable && 'cursor-pointer')}
                            onClick={() => focus(n)}
                          />
                          <text
                            x={props.x + props.width + 6}
                            y={props.y + props.height / 2 - 3}
                            fontSize={11}
                            fontWeight={500}
                            paintOrder="stroke"
                            stroke="var(--card)"
                            strokeOpacity={0.85}
                            strokeWidth={2.5}
                            strokeLinejoin="round"
                            className={clickable ? 'cursor-pointer fill-foreground' : 'fill-muted-foreground'}
                            onClick={() => focus(n)}
                          >
                            <tspan data-flow-name="true">{flowShortName(nodeName(n))}</tspan>
                            <tspan
                              x={props.x + props.width + 6}
                              dy={15}
                              fontSize={10}
                              fontWeight={400}
                              strokeWidth={1.75}
                              className="fill-muted-foreground"
                            >
                              {flowShortName(secondary)}
                            </tspan>
                          </text>
                        </Layer>
                      </g>
                    )
                  }}
                >
                  <Tooltip
                    content={({ active, payload }) => {
                      if (!active || !payload?.length) return null
                      // 优先使用类型完备的悬停节点/路径状态渲染信息更丰富的极简浮层
                      if (hoveredNode !== null && graph.nodes[hoveredNode]) {
                        const n = graph.nodes[hoveredNode]
                        const identity = flowIdentity(n, t)
                        const share = totalValue > 0 ? formatBp(Math.round((n.value / totalValue) * 10_000), locale) : null
                        return (
                          <div className="note-body max-w-80 px-3 py-2.5 text-xs">
                            <div className="flex items-center justify-between gap-4 border-b border-border/60 pb-1.5 text-[11px] text-muted-foreground">
                              <span className="inline-flex items-center gap-1.5 font-medium">
                                <span className="h-2 w-2 rounded-full" style={{ backgroundColor: n.other ? OTHER_COLOR : stageColor(n.stage) }} />
                                {stageLabel[n.stage]}
                              </span>
                              {share && <span className="tabular-nums">{t('analytics:flowShareTotal')} {share}</span>}
                            </div>
                            <div className="mt-1.5 flex items-baseline justify-between gap-4">
                              <span className="truncate font-medium text-foreground">{identity.primary}</span>
                              <span className="shrink-0 font-semibold text-foreground tabular-nums">{fmt(n.value)}</span>
                            </div>
                            {identity.detail && (
                              <p className="mt-0.5 truncate text-[11px] text-muted-foreground">{identity.detail}</p>
                            )}
                            {nodeClickable(n) && (
                              <p className="mt-1.5 border-t border-border/50 pt-1.5 text-[11px] text-muted-foreground/85">
                                {t('analytics:flowClickFilter')}
                              </p>
                            )}
                          </div>
                        )
                      }
                      if (hoveredLink !== null && graph.links[hoveredLink]) {
                        const edge = graph.links[hoveredLink]
                        const src = graph.nodes[edge.source]
                        const dst = graph.nodes[edge.target]
                        if (src && dst) {
                          const totalShare = totalValue > 0 ? formatBp(Math.round((edge.value / totalValue) * 10_000), locale) : null
                          const srcShare = src.value > 0 ? formatBp(Math.round((edge.value / src.value) * 10_000), locale) : null
                          return (
                            <div className="note-body max-w-80 px-3 py-2.5 text-xs">
                              <div className="flex items-center gap-1.5 border-b border-border/60 pb-1.5 text-[11px] text-muted-foreground">
                                <span className="h-2 w-2 rounded-full" style={{ backgroundColor: src.other ? OTHER_COLOR : stageColor(src.stage) }} />
                                <span>{stageLabel[src.stage]}</span>
                                <span aria-hidden>→</span>
                                <span className="h-2 w-2 rounded-full" style={{ backgroundColor: dst.other ? OTHER_COLOR : stageColor(dst.stage) }} />
                                <span>{stageLabel[dst.stage]}</span>
                              </div>
                              <p className="mt-1.5 break-all font-medium text-foreground">
                                {nodeName(src)} → {nodeName(dst)}
                              </p>
                              <div className="mt-1.5 flex flex-wrap items-baseline justify-between gap-x-4 gap-y-1 tabular-nums">
                                <span className="font-semibold text-foreground">{fmt(edge.value)}</span>
                                <span className="text-[11px] text-muted-foreground">
                                  {[
                                    totalShare ? `${t('analytics:flowShareTotal')} ${totalShare}` : null,
                                    srcShare ? `${t('analytics:flowShareSource')} ${srcShare}` : null,
                                  ].filter(Boolean).join(' · ')}
                                </span>
                              </div>
                            </div>
                          )
                        }
                      }
                      const item = payload[0]
                      return (
                        <div className="note-body max-w-80 px-3 py-2 text-xs">
                          <span className="font-medium text-foreground">{String(item?.name ?? '')}</span>
                          {item?.value != null && (
                            <span className="ml-2 font-semibold text-foreground tabular-nums">{fmt(Number(item.value))}</span>
                          )}
                        </div>
                      )
                    }}
                  />
                </Sankey>
              </ResponsiveContainer>
            </div>
          </div>
        )}

        {hasNegativeFlows && <p role="note" className="rounded-xl border border-warning/30 bg-warning/10 px-3.5 py-2.5 text-sm text-warning">{t('flow:netRefundHint')}</p>}
        {missingNames && <p className="text-xs text-muted-foreground">{t('flow:missingHint')}</p>}
        {detailNodes.length > 0 && (
          <details open={hasNegativeFlows || undefined} className="group/flow-details overflow-hidden rounded-xl border border-border/80 bg-card">
            <summary className="flex cursor-pointer list-none items-center gap-2 bg-muted/20 px-3.5 py-2.5 text-sm font-medium outline-none transition-colors hover:bg-muted/40 focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-primary/40 [&::-webkit-details-marker]:hidden">
              <ChevronRight aria-hidden className="h-4 w-4 shrink-0 text-muted-foreground transition-transform duration-150 group-open/flow-details:rotate-90" />
              <span>{t('flow:details')} · {detailNodes.length}</span>
            </summary>
            {actualStages.length > 1 && (
              <div className="flex flex-wrap items-center gap-1.5 border-t border-border/60 bg-muted/10 px-3.5 py-2 text-xs">
                <button
                  type="button"
                  onClick={() => setDetailStage('all')}
                  className={cn(
                    'h-6 cursor-pointer rounded-md px-2 transition-colors outline-none focus-visible:ring-2 focus-visible:ring-primary/40',
                    effectiveDetailStage === 'all'
                      ? 'bg-card font-medium text-foreground shadow-xs ring-1 ring-inset ring-border/70'
                      : 'text-muted-foreground hover:text-foreground',
                  )}
                >
                  {t('analytics:flowAllStages')}
                </button>
                {actualStages.map((stage) => {
                  const active = effectiveDetailStage === stage
                  return (
                    <button
                      key={stage}
                      type="button"
                      onClick={() => setDetailStage(stage)}
                      className={cn(
                        'inline-flex h-6 cursor-pointer items-center gap-1.5 rounded-md px-2 transition-colors outline-none focus-visible:ring-2 focus-visible:ring-primary/40',
                        active
                          ? 'bg-card font-medium text-foreground shadow-xs ring-1 ring-inset ring-border/70'
                          : 'text-muted-foreground hover:text-foreground',
                      )}
                    >
                      <span aria-hidden className="h-1.5 w-1.5 rounded-full" style={{ backgroundColor: stageColor(stage) }} />
                      {stageLabel[stage]}
                    </button>
                  )
                })}
              </div>
            )}
            <Table wrapperClassName="rounded-none border-0 border-t border-border/70 shadow-none">
              <THead>
                <Tr>
                  <Th>{t('analysis:stages')}</Th>
                  <Th>{t('flow:name')}</Th>
                  <Th>{t('flow:identity')}</Th>
                  <Th numeric>{metricLabel[metric]}</Th>
                  <Th numeric>{t('flow:share')}</Th>
                </Tr>
              </THead>
              <TBody>
                {visibleDetailStages.flatMap((stage) => detailNodes.filter((n) => n.stage === stage).sort((a, b) => b.value - a.value)).map((n) => (
                  <Tr key={n.id}>
                    <Td>
                      <span className="inline-flex items-center gap-1.5 whitespace-nowrap text-xs text-muted-foreground">
                        <span aria-hidden className="h-2 w-2 shrink-0 rounded-full" style={{ backgroundColor: stageColor(n.stage) }} />
                        {stageLabel[n.stage]}
                      </span>
                    </Td>
                    <Td>
                      <button
                        type="button"
                        className="cursor-pointer rounded-sm text-left font-medium outline-none transition-colors hover:text-primary hover:underline hover:underline-offset-4 focus-visible:ring-2 focus-visible:ring-primary/40 disabled:cursor-default disabled:text-muted-foreground disabled:no-underline"
                        disabled={n.other || !n.key || n.key === '0'}
                        onClick={() => focus(n)}
                      >
                        {nodeName(n)}
                      </button>
                    </Td>
                    <Td className="text-xs text-muted-foreground">{flowIdentity(n, t).detail || '—'}</Td>
                    <Td numeric className="font-medium">{fmt(n.value)}</Td>
                    <Td numeric className="text-muted-foreground">{totalValue ? formatBp(Math.round(n.value / totalValue * 10_000), locale) : '—'}</Td>
                  </Tr>
                ))}
              </TBody>
            </Table>
          </details>
        )}
      </div>

      <div className="border-t border-border/60 bg-muted/20 px-4 py-2.5 text-xs text-muted-foreground sm:px-5">
        {t('analytics:flowHint', { n: search.limit ?? 6 })} {t('analysis:stageHint')}
      </div>
    </Card>
  )
}
