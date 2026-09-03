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
import { cleanSearch, cubeParams } from '@/features/analytics/search'
import type { FlowNode, FlowResp } from '@/features/analytics/types'
import { apiFetch } from '@/lib/api'
import { OTHER_COLOR, chartColor } from '@/lib/chart'
import { describeError } from '@/lib/i18n'
import { formatBp, formatCount, formatMoneyAggregate } from '@/lib/money'
import { qk } from '@/lib/query-keys'

const STAGE_ORDER: FlowNode['stage'][] = ['user', 'api_key', 'group', 'model', 'channel']

/// 阶段配色：同一阶段同色，跨阶段用分类色板的相邻色——读者按列识别阶段，
/// 而不是按节点识别实体（实体名写在节点旁）。
function stageColor(stage: FlowNode['stage']): string {
  return chartColor(STAGE_ORDER.indexOf(stage))
}

/// 流向视图：用户 → 密钥 → 分组 → 模型 → 渠道 的桑基图（new-api 数据看板 Flow 的对应物）。
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
  const params = cubeParams(search, { metric, limit: '6' })
  const q = useQuery({
    queryKey: qk.statsFlow(params),
    queryFn: () => apiFetch<FlowResp>(`/admin/stats/flow?${params}`),
    retry: false,
  })

  const metricLabel: Record<FlowMetric, string> = {
    amount: t('analytics:kpiSpend'),
    requests: t('common:requests'),
    tokens: t('common:tokens'),
  }
  const stageLabel: Record<FlowNode['stage'], string> = {
    user: t('analytics:dimUser'),
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

  const nodeName = (n: FlowNode): string => {
    if (n.other) return t('analytics:other')
    if (n.label) return n.label
    return n.stage === 'model' || n.stage === 'group' ? n.key : `#${n.key}`
  }
  // 节点旁只放得下二十来个字符：长名字截断，完整名进 <title>（悬停可见）
  const shortName = (n: FlowNode): string => {
    const s = nodeName(n)
    return s.length > 22 ? `${s.slice(0, 21)}…` : s
  }

  const focus = (n: FlowNode) => {
    if (n.other) return
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

        {/* 列头：让读者先知道五列各是什么，再读节点 */}
        <div className="grid grid-cols-5 text-center text-[11px] text-muted-foreground">
          {STAGE_ORDER.map((s) => (
            <span key={s} className="flex items-center justify-center gap-1">
              <span className="inline-block h-2 w-2 rounded-sm" style={{ background: stageColor(s) }} />
              {stageLabel[s]}
            </span>
          ))}
        </div>

        {q.isError ? (
          <ErrorState message={describeError(q.error)} />
        ) : q.isLoading || graph === null ? (
          <LoadingState />
        ) : graph.nodes.length === 0 || graph.links.length === 0 ? (
          <EmptyState hint={t('admin:trendEmptyHint')} />
        ) : (
          <div className="h-[28rem]">
            <ResponsiveContainer width="100%" height="100%">
              <Sankey
                data={{
                  nodes: graph.nodes.map((n) => ({ name: nodeName(n) })),
                  links: graph.links,
                }}
                nodeWidth={12}
                nodePadding={14}
                // 右侧留出最后一列标签的位置：五列标签统一放节点右侧，避免相邻两列的
                // 左/右标签在同一条空隙里对撞
                margin={{ top: 8, right: 190, bottom: 8, left: 8 }}
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
                  const clickable = !n.other
                  return (
                    <Layer key={n.id}>
                      <title>{`${nodeName(n)} · ${fmt(n.value)}`}</title>
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
                      {/* 太矮的节点不写字：相邻几个 1% 的节点各写一行会叠成一团，悬停 title 仍可见 */}
                      {props.height >= 9 && (
                        <text
                          x={props.x + props.width + 6}
                          y={props.y + props.height / 2}
                          textAnchor="start"
                          dominantBaseline="middle"
                          fontSize={11}
                          className={clickable ? 'cursor-pointer fill-foreground' : 'fill-muted-foreground'}
                          onClick={() => focus(n)}
                        >
                          {shortName(n)}
                          <tspan className="fill-muted-foreground" dx={4}>
                            {fmt(n.value)}
                          </tspan>
                        </text>
                      )}
                    </Layer>
                  )
                }}
              >
                <Tooltip
                  formatter={(v) => fmt(Number(v))}
                  contentStyle={{ fontSize: 12 }}
                />
              </Sankey>
            </ResponsiveContainer>
          </div>
        )}
        <p className="text-xs text-muted-foreground">{t('analytics:flowHint')}</p>
      </CardContent>
    </Card>
  )
}
