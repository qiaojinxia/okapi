import { useQuery } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { CheckCircle2, ChevronRight } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import type { ReconResp } from '@/features/ops/ReconciliationCard'
import { apiFetch } from '@/lib/api'
import { qk } from '@/lib/query-keys'

/// 与 /admin/stats/channels 的行形状对齐（渠道名字段是 `name`，不是 `channel_name`——
/// 此前写错字段，待办文案里的"如 xxx"永远是空括号）。
interface ChannelStat {
  channel_id: number
  name: string
  error_rate_bp: number
}
interface ModelRow {
  model_name: string
  pricing_mode: string | null
}
interface PoolRow {
  pool_code: string
  channel_count: number
}
interface Diagnose {
  postgres: boolean
  redis: boolean
  /// null = 未启用（不是故障）
  clickhouse: boolean | null
  nats_connected: boolean
  outbox_pending: number
  dlq_depth: number
  cooling_keys: number
  pricebook_epoch: number
}
/// 一条待办。`to` 指向能解决它的页面——发现问题和处理问题之间不该再让人找路。
interface Item {
  key: string
  text: string
  to: string
  tone: 'warning' | 'destructive'
}

/// 组件状态芯片行：四个绿点比四行文字快得多；未启用的组件显示为灰而非红——
/// 单机形态没有 NATS/CH 是正常配置，不是故障。
function HealthChips({ h }: { h: Diagnose }) {
  const { t } = useTranslation()
  const chips: { name: string; state: 'ok' | 'down' | 'off' }[] = [
    { name: 'PG', state: h.postgres ? 'ok' : 'down' },
    { name: 'Redis', state: h.redis ? 'ok' : 'down' },
    { name: 'CH', state: h.clickhouse === null ? 'off' : h.clickhouse ? 'ok' : 'down' },
    { name: 'NATS', state: h.nats_connected ? 'ok' : 'off' },
  ]
  return (
    <div className="flex items-center gap-2" title={t('admin:healthEpoch', { n: h.pricebook_epoch })}>
      {chips.map((c) => (
        <span key={c.name} className="inline-flex items-center gap-1 text-xs text-muted-foreground">
          <span
            className={
              c.state === 'ok'
                ? 'h-2 w-2 rounded-full bg-success'
                : c.state === 'down'
                  ? 'h-2 w-2 rounded-full bg-destructive'
                  : 'h-2 w-2 rounded-full bg-muted-foreground/40'
            }
          />
          {c.name}
        </span>
      ))}
    </div>
  )
}

/// "需要注意"面板。
///
/// 落地页的价值不是把所有数字摆出来，而是回答"我现在该做什么"。此前这里是一张
/// 三方对账漂移表——那是排障工具，不是日常动线，且没有漂移时整页空着。
/// 现在只列真正需要动手的项，全清时明确说"没有待办"而不是留一片空白。
export function AttentionCard({ days }: { days: number }) {
  const { t } = useTranslation()

  const channels = useQuery({
    queryKey: qk.statsChannels(days),
    queryFn: () => apiFetch<{ data: ChannelStat[] }>(`/admin/stats/channels?days=${days}`),
  })
  const models = useQuery({
    queryKey: qk.adminModels,
    queryFn: () => apiFetch<{ data: ModelRow[] }>('/admin/models'),
  })
  const pools = useQuery({
    queryKey: qk.adminPools,
    queryFn: () => apiFetch<{ data: PoolRow[] }>('/admin/pools'),
  })
  const drift = useQuery({
    queryKey: qk.reconciliation,
    // 该端点返回 { drift_count, drifts }，不是通用的 { data } 形状
    queryFn: () => apiFetch<ReconResp>('/admin/reconciliation'),
  })
  // 全链路健康（与 MCP diagnose 同一函数）：组件不可达与积压是最紧急的一类待办，
  // 30s 轮询——它们变化的时间尺度是分钟，不需要秒级
  const health = useQuery({
    queryKey: qk.diagnose,
    queryFn: () => apiFetch<Diagnose>('/admin/diagnose'),
    refetchInterval: 30_000,
  })

  const items: Item[] = []

  // 组件不可达：账本链路（PG/Redis）挂了付费请求全部 fail-closed，是最高优先级
  const h = health.data
  if (h) {
    const down: string[] = []
    if (!h.postgres) down.push('PostgreSQL')
    if (!h.redis) down.push('Redis')
    if (h.clickhouse === false) down.push('ClickHouse')
    if (down.length > 0) {
      items.push({
        key: 'component-down',
        text: t('admin:attnComponentDown', { names: down.join(' / ') }),
        to: '/admin/ops',
        tone: 'destructive',
      })
    }
    // DLQ 有死信 = 有账写不进 CH，统计口径已开始漂移；outbox 积压 = worker 没跟上
    if (h.dlq_depth > 0) {
      items.push({
        key: 'dlq',
        text: t('admin:attnDlq', { n: h.dlq_depth }),
        to: '/admin/ops',
        tone: 'destructive',
      })
    }
    if (h.outbox_pending >= 1_000) {
      items.push({
        key: 'outbox',
        text: t('admin:attnOutbox', { n: h.outbox_pending }),
        to: '/admin/ops',
        tone: 'warning',
      })
    }
    if (h.cooling_keys > 0) {
      items.push({
        key: 'cooling',
        text: t('admin:attnCooling', { n: h.cooling_keys }),
        to: '/admin/channels',
        tone: 'warning',
      })
    }
  }

  // 未定价模型：建了模型却没配价，请求会被直接拒——属于"配了一半"的典型漏项
  const unpriced = (models.data?.data ?? []).filter((m) => m.pricing_mode === null)
  if (unpriced.length > 0) {
    items.push({
      key: 'unpriced',
      text: t('admin:attnUnpriced', { n: unpriced.length, first: unpriced[0]?.model_name ?? '' }),
      to: '/admin/pricing',
      tone: 'destructive',
    })
  }

  // 空池：分组指向它就等于该组无可用渠道，症状是 503 而原因很不直观
  const emptyPools = (pools.data?.data ?? []).filter((p) => p.channel_count === 0)
  if (emptyPools.length > 0) {
    items.push({
      key: 'empty-pool',
      text: t('admin:attnEmptyPool', {
        n: emptyPools.length,
        first: emptyPools[0]?.pool_code ?? '',
      }),
      to: '/admin/pools',
      tone: 'warning',
    })
  }

  // 高错误率渠道：5% 起视为需要处置（与渠道健康卡同阈值）
  const bad = (channels.data?.data ?? []).filter((c) => c.error_rate_bp >= 500)
  if (bad.length > 0) {
    items.push({
      key: 'bad-channel',
      text: t('admin:attnBadChannel', {
        n: bad.length,
        // 示例优先挑有名字的渠道；id=0 是"无渠道"的聚合桶，"如 #0"对人没有信息量
        first:
          bad.find((c) => c.name)?.name ??
          `#${bad.find((c) => c.channel_id > 0)?.channel_id ?? bad[0]?.channel_id ?? ''}`,
      }),
      to: '/admin/channels',
      tone: 'destructive',
    })
  }

  // 对账漂移：Redis/PG/CH 三方口径不一致，属于要人工介入的账目问题
  const drifted = drift.data?.drifts ?? []
  if (drifted.length > 0) {
    items.push({
      key: 'drift',
      text: t('admin:attnDrift', { n: drifted.length }),
      to: '/admin/ops',
      tone: 'destructive',
    })
  }

  return (
    <Card>
      <CardHeader className="flex-row items-center justify-between">
        <CardTitle>{t('admin:attnTitle')}</CardTitle>
        {h && <HealthChips h={h} />}
      </CardHeader>
      <CardContent className="flex flex-col gap-2">
        {items.length === 0 ? (
          <div className="flex items-center gap-2 py-4 text-sm text-muted-foreground">
            <CheckCircle2 className="h-4 w-4 text-success" />
            {t('admin:attnClear')}
          </div>
        ) : (
          items.map((item) => (
            <Link
              key={item.key}
              to={item.to}
              className="flex items-center justify-between gap-3 rounded-md border border-border px-3 py-2.5 transition-colors hover:bg-muted/50"
            >
              <span className="flex min-w-0 items-start gap-2">
                <Badge variant={item.tone} className="mt-0.5 shrink-0">
                  {t(item.tone === 'destructive' ? 'admin:attnUrgent' : 'admin:attnNotice')}
                </Badge>
                {/* 待办文字宁可两行也不截断：被截掉的正是"哪 16 条、该去哪修" */}
                <span className="line-clamp-2 text-sm">{item.text}</span>
              </span>
              <ChevronRight className="h-4 w-4 shrink-0 text-muted-foreground" />
            </Link>
          ))
        )}
      </CardContent>
    </Card>
  )
}
