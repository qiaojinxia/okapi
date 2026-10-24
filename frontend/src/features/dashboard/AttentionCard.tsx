import { useQuery } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { ChevronRight, TriangleAlert } from 'lucide-react'
import { useEffect, useId, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { Tooltip } from '@/components/ui/tooltip'
import type { ReconResp } from '@/features/ops/ReconciliationCard'
import { apiFetch } from '@/lib/api'
import { qk } from '@/lib/query-keys'
import { cn } from '@/lib/utils'
import type { QualitySearch } from '@/features/quality/search'
import { useDashboardInventory } from './data'
import { maskEmails } from '@/lib/mask'

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
  summary: string
  text: string
  /// 行末入口的文字（“查看渠道”），去向由 `to` 决定。
  action: string
  to: string
  tone: 'warning' | 'destructive'
  search?: QualitySearch
}

function useDashboardHealth() {
  return useQuery({
    queryKey: qk.diagnose,
    queryFn: () => apiFetch<Diagnose>('/admin/diagnose'),
    refetchInterval: 30_000,
    retry: false,
  })
}

/// 页头副行里的服务状态：“服务 4/4 正常”。未启用的组件不计入分母，也不算故障——
/// 单机形态没有 NATS / CH 是正常配置；各组件逐项状态放进悬停说明和读屏名称。
export function HealthSummary() {
  const { t } = useTranslation()
  const health = useDashboardHealth()
  const h = health.isError ? undefined : health.data
  const frame = (children: React.ReactNode) => <span role="region" aria-label={t('admin:dashboardSystemHealth')} className="inline-flex">{children}</span>
  if (health.isError) return frame(<a href="#dashboard-attention" className="rounded text-warning underline underline-offset-2 outline-none focus-visible:ring-2 focus-visible:ring-primary/40">{t('admin:dashboardSystemUnavailable')}</a>)
  if (!h) return frame(<span role="status">{t('admin:dashboardChecking')}</span>)
  const parts: { name: string; state: 'ok' | 'down' | 'off' }[] = [
    { name: 'PG', state: h.postgres ? 'ok' : 'down' },
    { name: 'Redis', state: h.redis ? 'ok' : 'down' },
    { name: 'CH', state: h.clickhouse === null ? 'off' : h.clickhouse ? 'ok' : 'down' },
    { name: 'NATS', state: h.nats_connected ? 'ok' : 'off' },
  ]
  const enabled = parts.filter((part) => part.state !== 'off')
  const down = enabled.filter((part) => part.state === 'down')
  const detail = [...parts.map((part) => `${part.name}: ${t(`admin:dashboardHealth_${part.state}`)}`), t('admin:healthEpoch', { n: h.pricebook_epoch })].join(' · ')
  return frame(<Tooltip content={detail}>
    <span tabIndex={0} className={cn('rounded outline-none focus-visible:ring-2 focus-visible:ring-primary/40', down.length > 0 && 'font-medium text-destructive')}>
      {down.length > 0 ? t('admin:dashboardServicesDown', { names: down.map((part) => part.name).join(' / ') }) : t('admin:dashboardServicesOk', { ok: enabled.length, total: enabled.length })}
      <span className="sr-only"> ({detail})</span>
    </span>
  </Tooltip>)
}

/// 待办汇总。
///
/// 落地页的价值不是把所有数字摆出来，而是回答"我现在该做什么"。此前这里是一张
/// 三方对账漂移表——那是排障工具，不是日常动线，且没有漂移时整页空着。
/// 现在只列真正需要动手的项；全清时待办条整条不出现（页头的服务状态已说明一切正常），
/// 检查没做完或失败时不能当成“全清”，由待办条保留提示和重试。
function useAttention(days: number) {
  const { t } = useTranslation()
  const inventory = useDashboardInventory()

  const channels = useQuery({
    queryKey: qk.statsChannels(days),
    queryFn: () => apiFetch<{ data: ChannelStat[] }>(`/admin/stats/channels?days=${days}`),
    retry: false,
  })
  const models = useQuery({
    queryKey: qk.adminModels,
    queryFn: () => apiFetch<{ data: ModelRow[] }>('/admin/models'),
    retry: false,
  })
  const pools = useQuery({
    queryKey: qk.adminPools,
    queryFn: () => apiFetch<{ data: PoolRow[] }>('/admin/pools'),
    retry: false,
  })
  const drift = useQuery({
    queryKey: qk.reconciliation,
    // 该端点返回 { drift_count, drifts }，不是通用的 { data } 形状
    queryFn: () => apiFetch<ReconResp>('/admin/reconciliation'),
    retry: false,
  })
  // 全链路健康（与 MCP diagnose 同一函数）：组件不可达与积压是最紧急的一类待办，
  // 30s 轮询——它们变化的时间尺度是分钟，不需要秒级
  const health = useDashboardHealth()

  const items: Item[] = []

  // 组件不可达：账本链路（PG/Redis）挂了付费请求全部 fail-closed，是最高优先级
  const h = health.isError ? undefined : health.data
  if (h) {
    const down: string[] = []
    if (!h.postgres) down.push('PostgreSQL')
    if (!h.redis) down.push('Redis')
    if (h.clickhouse === false) down.push('ClickHouse')
    if (down.length > 0) {
      items.push({
        key: 'component-down',
        summary: t('admin:dashboardConnectionIssue'),
        text: t('admin:attnComponentDown', { names: down.join(' / ') }),
        action: t('admin:attnGoOps'),
        to: '/admin/ops',
        tone: 'destructive',
      })
    }
    // DLQ 有死信 = 有账写不进 CH，统计口径已开始漂移；outbox 积压 = worker 没跟上
    if (h.dlq_depth > 0) {
      items.push({
        key: 'dlq',
        summary: t('admin:dashboardLedgerIssue'),
        text: t('admin:attnDlq', { n: h.dlq_depth }),
        action: t('admin:attnGoOps'),
        to: '/admin/ops',
        tone: 'destructive',
      })
    }
    if (h.outbox_pending >= 1_000) {
      items.push({
        key: 'outbox',
        summary: t('admin:dashboardBacklogIssue'),
        text: t('admin:attnOutbox', { n: h.outbox_pending }),
        action: t('admin:attnGoOps'),
        to: '/admin/ops',
        tone: 'warning',
      })
    }
    if (h.cooling_keys > 0) {
      items.push({
        key: 'cooling',
        summary: t('admin:dashboardKeyIssue'),
        text: t('admin:attnCooling', { n: h.cooling_keys }),
        action: t('admin:attnGoChannels'),
        to: '/admin/channels',
        tone: 'warning',
      })
    }
  }

  // 未定价模型：建了模型却没配价，请求会被直接拒——属于"配了一半"的典型漏项
  const unpriced = (models.isError ? [] : models.data?.data ?? []).filter((m) => m.pricing_mode === null)
  const stock = inventory.isError ? undefined : inventory.data
  const unpricedCount = stock ? Math.max(0, stock.models.total - stock.models.priced) : unpriced.length
  if (unpricedCount > 0) {
    items.push({
      key: 'unpriced',
      summary: t('admin:dashboardPricingIssue'),
      text: unpriced[0]?.model_name ? t('admin:attnUnpriced', { n: unpricedCount, first: unpriced[0].model_name }) : t('admin:dashboardUnpricedAction', { n: unpricedCount }),
      action: t('admin:attnGoPricing'),
      to: '/admin/pricing',
      tone: 'destructive',
    })
  }

  // 与首屏资源使用同一份汇总，避免渠道已停用时页头仍然显示“暂无待办”。
  const channelIssues = [
    stock && stock.channels.auto_disabled > 0 ? t('admin:invChannelsAutoDisabled', { n: stock.channels.auto_disabled }) : '',
    stock && stock.channels.no_key > 0 ? t('admin:invChannelsNoKey', { n: stock.channels.no_key }) : '',
  ].filter(Boolean)
  if (stock && channelIssues.length > 0) {
    items.push({
      key: 'channel-availability',
      summary: t('admin:dashboardAvailabilityIssue'),
      text: t('admin:dashboardAvailabilityAction', { issues: channelIssues.join('；') }),
      action: t('admin:attnGoChannels'),
      to: '/admin/channels',
      tone: stock.channels.auto_disabled > 0 ? 'destructive' : 'warning',
    })
  }

  // 空池：分组指向它就等于该组无可用渠道，症状是 503 而原因很不直观
  const emptyPools = (pools.data?.data ?? []).filter((p) => p.channel_count === 0)
  if (emptyPools.length > 0) {
    items.push({
      key: 'empty-pool',
      summary: t('admin:dashboardPoolIssue'),
      text: t('admin:attnEmptyPool', {
        n: emptyPools.length,
        first: emptyPools[0]?.pool_code ?? '',
      }),
      action: t('admin:attnGoPools'),
      to: '/admin/pools',
      tone: 'warning',
    })
  }

  // 高错误率渠道：5% 起视为需要处置（与渠道健康卡同阈值）
  const bad = (channels.data?.data ?? []).filter((c) => c.error_rate_bp >= 500)
  if (bad.length > 0) {
    items.push({
      key: 'bad-channel',
      summary: t('admin:dashboardChannelIssue'),
      text: t('admin:attnBadChannel', {
        n: bad.length,
        // 示例优先挑有名字的渠道；id=0 是"无渠道"的聚合桶，"如 #0"对人没有信息量
        first: maskEmails(bad.find((c) => c.name)?.name ?? '') || (bad.some((c) => c.channel_id > 0)
          ? t('admin:dashboardUnnamedChannel', { id: bad.find((c) => c.channel_id > 0)!.channel_id })
          : t('admin:dashboardUnassignedChannel')),
      }),
      action: t('admin:attnGoChannels'),
      to: '/admin/quality',
      search: { days, tab: 'channels' },
      tone: 'destructive',
    })
  }

  // 对账漂移：Redis/PG/CH 三方口径不一致，属于要人工介入的账目问题
  const drifted = drift.data?.drifts ?? []
  if (drifted.length > 0) {
    items.push({
      key: 'drift',
      summary: t('admin:dashboardBalanceIssue'),
      text: t('admin:attnDrift', { n: drifted.length }),
      action: t('admin:attnGoReconcile'),
      to: '/admin/ops',
      tone: 'destructive',
    })
  }

  const queries = [channels, models, pools, drift, health, inventory]
  const incomplete = queries.some((query) => query.isError)
  items.sort((a, b) => Number(b.tone === 'destructive') - Number(a.tone === 'destructive'))

  return { items, incomplete, retry: () => Promise.all(queries.filter((query) => query.isError).map((query) => query.refetch())) }
}

/// 待办条：一张卡，每项一行——严重 / 提醒标签、标题、说明，行末是去处理的入口。
/// 全部清空时不占位（页头的服务状态已经说明系统是好的）；检查没做完也不报“暂无待办”，
/// 检查失败则保留一行提示和重试。桌面每行两项，超过四项在原位展开。
export function AttentionStrip({ days }: { days: number }) {
  const { t } = useTranslation()
  const { items, incomplete, retry } = useAttention(days)
  const [expanded, setExpanded] = useState(false)
  const listId = useId()
  useEffect(() => { setExpanded(false) }, [days])
  if (items.length === 0 && !incomplete) return null
  const visible = expanded ? items : items.slice(0, 4)

  return <section id="dashboard-attention" tabIndex={-1} aria-label={t('admin:dashboardPriorityActions')}
    className="min-w-0 scroll-mt-4 overflow-hidden rounded-xl border border-border bg-card outline-none focus-visible:ring-2 focus-visible:ring-primary/40">
    {items.length > 0 && <div id={listId} className="flex flex-wrap gap-px bg-border">
      {visible.map((item) => {
        const critical = item.tone === 'destructive'
        return <Link key={item.key} to={item.to} search={item.search} aria-label={item.text}
          className={cn('group flex min-h-11 min-w-0 flex-1 basis-[22.5rem] items-center gap-3 bg-card px-3.5 py-2 text-[13px] outline-none hover:bg-muted/50 focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-primary/40', critical && 'border-l-[3px] border-l-destructive')}>
          <span className={cn('shrink-0 rounded-[5px] px-2 py-0.5 text-xs font-semibold whitespace-nowrap', critical ? 'bg-destructive/10 text-destructive' : 'bg-warning/15 text-warning')}>{critical ? t('admin:attnCritical') : t('admin:attnNotice')}</span>
          <span className="min-w-0 flex-1 leading-5"><b className="font-semibold">{item.summary}</b><span className="text-muted-foreground"> · {item.text}</span></span>
          <span className="shrink-0 whitespace-nowrap text-primary group-hover:underline">{item.action}<span aria-hidden> →</span></span>
        </Link>
      })}
    </div>}
    {items.length > 4 && <button type="button" aria-expanded={expanded} aria-controls={listId} onClick={() => setExpanded((value) => !value)}
      className="flex min-h-9 w-full items-center justify-center gap-1 border-t border-border text-xs text-muted-foreground outline-none hover:bg-muted/50 hover:text-primary focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-primary/40">
      {expanded ? t('admin:dashboardShowLess') : t('admin:dashboardAllIssues', { count: items.length })}<ChevronRight aria-hidden size={12} className={cn('transition-transform', expanded ? '-rotate-90' : 'rotate-90')} />
    </button>}
    {incomplete && <div role="alert" className={cn('flex min-w-0 items-center gap-2 px-3.5 py-2 text-[13px] text-muted-foreground', items.length > 0 && 'border-t border-border')}>
      <TriangleAlert aria-hidden className="h-3.5 w-3.5 shrink-0 text-warning" />
      <span className="min-w-0 flex-1">{t('admin:dashboardChecksIncomplete')}</span>
      <Button size="xs" variant="outline" onClick={() => void retry()}>{t('common:retry')}</Button>
    </div>}
  </section>
}
