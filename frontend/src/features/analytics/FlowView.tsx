import { useQuery } from '@tanstack/react-query'
import { useNavigate } from '@tanstack/react-router'
import { useMemo } from 'react'
import { useTranslation } from 'react-i18next'
import { Layer, Rectangle, ResponsiveContainer, Sankey, Tooltip } from 'recharts'
import type { AnalyticsSearch, FlowMetric } from '@/routes/admin.stats'
import { FLOW_METRICS } from '@/routes/admin.stats'
import { Card, CardContent } from '@/components/ui/card'
import { Segmented } from '@/components/ui/segmented'
import { EmptyState, ErrorState, LoadingState } from '@/components/ui/state'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { flowIdentity, flowShortName } from './flow-labels'
import { selectClass } from './AnalysisControls'
import { cleanSearch, cubeParams } from '@/features/analytics/search'
import type { FlowNode, FlowResp } from '@/features/analytics/types'
import { apiFetch } from '@/lib/api'
import { OTHER_COLOR, chartColor } from '@/lib/chart'
import { describeError } from '@/lib/i18n'
import { formatBp, formatCount, formatMoneyAggregate } from '@/lib/money'
import { qk } from '@/lib/query-keys'

const STAGE_ORDER: FlowNode['stage'][] = ['user', 'node', 'api_key', 'group', 'model', 'channel']

/// 阶段配色：同一阶段同色，跨阶段用分类色板的相邻色——读者按列识别阶段，
/// 而不是按节点识别实体（实体名写在节点旁）。
function stageColor(stage: FlowNode['stage']): string {
  return chartColor(STAGE_ORDER.indexOf(stage))
}

/// 流向视图：用户 → 网关节点 → 密钥 → 分组 → 模型 → 渠道 的桑基图（new-api 数据看板 Flow 的对应物）。
///
/// 它回答别的图答不了的一类问题——"这条渠道的流量是谁打的""这个用户的钱经过
/// 哪些模型流向了哪几家上游"。每列只留 Top N，其余折成灰色"其他"；点击具名节点
/// 即把它设为过滤条件（与拆分表的"聚焦"同一动作）。覆盖率 < 100% 时明示：
/// 图是头部组合的流向，不是全量。
export function FlowView({ search }: { search: AnalyticsSearch }) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const navigate = useNavigate({ from: '/admin/stats' })
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
    metric === 'amount' ? formatMoneyAggregate(v, locale) : formatCount(v, locale)

  // Recharts 的 Sankey 按数组下标引用节点：把 id → 下标映射一次
  const graph = useMemo(() => {
    const resp = q.data
    if (!resp) return null
    const nodes = resp.nodes.filter((n) => n.value > 0)
    const index = new Map(nodes.map((n, i) => [n.id, i]))
    const links = resp.links
      .filter((l) => index.has(l.source) && index.has(l.target) && l.value > 0)
      .map((l) => ({ source: index.get(l.source) ?? 0, target: index.get(l.target) ?? 0, value: l.value }))
    return { nodes, links }
  }, [q.data])

  const nodeName = (n: FlowNode) => flowIdentity(n, t).primary
  const nodeDetail = (n: FlowNode) => {
    const identity = flowIdentity(n, t)
    return [stageLabel[n.stage], identity.primary, identity.detail, fmt(n.value), q.data?.total ? formatBp(Math.round(n.value / q.data.total * 10_000), locale) : ''].filter(Boolean).join(' · ')
  }
  const actualStages = STAGE_ORDER.filter((stage) => graph?.nodes.some((n) => n.stage === stage))
  const maxStageNodes = Math.max(1, ...actualStages.map((s) => graph?.nodes.filter((n) => n.stage === s).length ?? 0))
  const graphHeight = Math.max(420, maxStageNodes * 42 + 48)
  const missingNames = graph?.nodes.some((n) => flowIdentity(n, t).missing)

  const focus = (n: FlowNode) => {
    if (n.other || !n.key || (['user', 'api_key', 'channel'].includes(n.stage) && n.key === '0')) return
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
      case 'node': patch.node = n.key; break
      case 'group':
        patch.group = n.key
        break
      default:
        return
    }
    void navigate({ search: (prev) => cleanSearch({ ...prev, ...patch }) })
  }

  return (
    <Card>
      <CardContent className="flex flex-col gap-3 pt-4">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <div className="flex items-center gap-2">
            <span className="text-xs text-muted-foreground">{t('analytics:flowMetric')}</span>
            <Segmented
              options={FLOW_METRICS.map((m) => ({ value: m, label: metricLabel[m] }))}
              value={metric}
              onChange={(v) => void navigate({ search: (prev) => cleanSearch({ ...prev, metric: v }) })}
              size="sm"
            />
          </div>
          {q.data && q.data.coverage_bp < 9_950 && (
            <span className="text-xs text-warning">
              {t('analytics:flowCoverage', { v: formatBp(q.data.coverage_bp, locale) })}
            </span>
          )}
        </div>

        <div className="flex flex-wrap items-center gap-3 rounded-lg bg-muted/30 p-3 text-xs"><span className="text-muted-foreground">{t('analysis:stages')}</span>{STAGE_ORDER.map((stage) => <label key={stage} className="flex min-h-8 cursor-pointer items-center gap-1.5"><input type="checkbox" checked={stages.includes(stage)} disabled={stages.includes(stage) && stages.length <= 2} onChange={(e) => void navigate({ search: (prev) => cleanSearch({ ...prev, stages: e.target.checked ? STAGE_ORDER.filter((s) => stages.includes(s) || s === stage) : stages.filter((s) => s !== stage) }) })} />{stageLabel[stage]}</label>)}<label className="flex items-center gap-2">{t('analysis:top')}<select className={selectClass} value={search.limit ?? 6} onChange={(e) => void navigate({ search: (prev) => cleanSearch({ ...prev, limit: Number(e.target.value) }) })}>{[3, 6, 10, 20].map((n) => <option key={n}>{n}</option>)}</select></label></div>
        {q.isError ? (
          <ErrorState message={describeError(q.error)} />
        ) : q.isLoading || graph === null ? (
          <LoadingState />
        ) : graph.nodes.length === 0 || graph.links.length === 0 ? (
          <EmptyState hint={t('admin:trendEmptyHint')} />
        ) : (
          <div className="max-w-full overflow-x-auto"><div style={{ height: graphHeight, minWidth: Math.max(620, actualStages.length * 145 + 40) }}>
            <ResponsiveContainer width="100%" height="100%">
              <Sankey
                data={{
                  nodes: graph.nodes.map((n) => ({ name: [nodeName(n), flowIdentity(n, t).detail].filter(Boolean).join(' · ') })),
                  links: graph.links,
                }}
                nodeWidth={12}
                nodePadding={28}
                // 右侧留出两行标签的空间；标题由节点实际 x 坐标绘制，兼容不同阶段数。
                margin={{ top: 44, right: 148, bottom: 24, left: 8 }}
                // 链接按来源列着色：一眼能看出"这条带子从哪一列流出来"
                link={(props) => {
                  const sourceNode = graph.nodes[graph.links[props.index]?.source ?? -1]
                  const stroke = sourceNode && !sourceNode.other ? stageColor(sourceNode.stage) : OTHER_COLOR
                  const d = `M${props.sourceX},${props.sourceY} C${props.sourceControlX},${props.sourceY} ${props.targetControlX},${props.targetY} ${props.targetX},${props.targetY}`
                  return (
                    <path
                      d={d}
                      fill="none"
                      stroke={stroke}
                      strokeOpacity={0.22}
                      strokeWidth={Math.max(1, props.linkWidth)}
                    />
                  )
                }}
                node={(props) => {
                  const n = graph.nodes[props.index]
                  if (!n) return <g />
                  const fill = n.other ? OTHER_COLOR : stageColor(n.stage)
                  const clickable = !n.other && !!n.key && !(['user', 'api_key', 'channel'].includes(n.stage) && n.key === '0')
                  const identity = flowIdentity(n, t)
                  const firstInStage = graph.nodes.findIndex((node) => node.stage === n.stage) === props.index
                  const secondary = [identity.missing ? identity.id : '', fmt(n.value), identity.deleted ? t('flow:deleted') : ''].filter(Boolean).join(' · ')
                  return (
                    <g key={n.id}>
                      {firstInStage && <g data-flow-stage={n.stage}><circle cx={props.x + 4} cy={18} r={3.5} fill={stageColor(n.stage)} /><text x={props.x + 14} y={22} fontSize={11} className="fill-muted-foreground">{stageLabel[n.stage]}</text></g>}
                    <Layer data-flow-node={n.id} role={clickable ? 'button' : undefined} tabIndex={clickable ? 0 : undefined} aria-label={nodeDetail(n)} onKeyDown={(e) => { if (clickable && (e.key === 'Enter' || e.key === ' ')) { e.preventDefault(); focus(n) } }}>
                      <title>{nodeDetail(n)}</title>
                      <Rectangle
                        x={props.x}
                        y={props.y}
                        width={props.width}
                        height={props.height}
                        fill={fill}
                        fillOpacity={n.other ? 0.5 : 0.9}
                        radius={2}
                        className={clickable ? 'cursor-pointer' : undefined}
                        onClick={() => focus(n)}
                      />
                      <text x={props.x + props.width + 6} y={props.y + props.height / 2 - 3}
                        fontSize={11} fontWeight={500} className={clickable ? 'cursor-pointer fill-foreground' : 'fill-muted-foreground'} onClick={() => focus(n)}>
                        <tspan data-flow-name="true">{flowShortName(nodeName(n))}</tspan>
                        <tspan x={props.x + props.width + 6} dy={15} fontSize={10} fontWeight={400} className="fill-muted-foreground">{flowShortName(secondary)}</tspan>
                      </text>
                    </Layer>
                    </g>
                  )
                }}
              >
                <Tooltip
                  formatter={(v) => fmt(Number(v))}
                  contentStyle={{ fontSize: 12, maxWidth: 360, whiteSpace: 'normal', overflowWrap: 'anywhere', background: 'var(--color-popover)', color: 'var(--color-popover-foreground)', borderColor: 'var(--color-border)', borderRadius: 10 }}
                />
              </Sankey>
            </ResponsiveContainer>
          </div></div>
        )}
        {missingNames && <p className="text-xs text-muted-foreground">{t('flow:missingHint')}</p>}
        {graph && graph.nodes.length > 0 && <details className="rounded-lg border border-border"><summary className="cursor-pointer px-3 py-2 text-sm font-medium">{t('flow:details')} · {graph.nodes.length}</summary><Table><THead><Tr><Th>{t('analysis:stages')}</Th><Th>{t('flow:name')}</Th><Th>{t('flow:identity')}</Th><Th>{metricLabel[metric]}</Th><Th>{t('flow:share')}</Th></Tr></THead><TBody>{actualStages.flatMap((stage) => graph.nodes.filter((n) => n.stage === stage).sort((a, b) => b.value - a.value)).map((n) => <Tr key={n.id}><Td>{stageLabel[n.stage]}</Td><Td><button type="button" className="text-left font-medium hover:text-primary disabled:cursor-default disabled:text-muted-foreground" disabled={n.other || !n.key || n.key === '0'} onClick={() => focus(n)}>{nodeName(n)}</button></Td><Td className="text-xs text-muted-foreground">{flowIdentity(n, t).detail || '—'}</Td><Td>{fmt(n.value)}</Td><Td>{q.data?.total ? formatBp(Math.round(n.value / q.data.total * 10_000), locale) : '—'}</Td></Tr>)}</TBody></Table></details>}
        <p className="text-xs text-muted-foreground">{t('analytics:flowHint', { n: search.limit ?? 6 })} {t('analysis:stageHint')}</p>
      </CardContent>
    </Card>
  )
}
