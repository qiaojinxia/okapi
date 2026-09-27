import { useQuery } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { CheckCircle2, ChevronRight, TriangleAlert } from 'lucide-react'
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
  to: string
  tone: 'warning' | 'destructive'
  search?: QualitySearch
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
    <div className="flex flex-wrap items-center gap-x-2 gap-y-1" title={t('admin:healthEpoch', { n: h.pricebook_epoch })}>
      {chips.map((c) => (
        <span key={c.name} aria-label={`${c.name}: ${t(`admin:dashboardHealth_${c.state}`)}`} title={t(`admin:dashboardHealth_${c.state}`)} className="inline-flex items-center gap-1 text-xs text-muted-foreground">
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

function useDashboardHealth() {
  return useQuery({
    queryKey: qk.diagnose,
    queryFn: () => apiFetch<Diagnose>('/admin/diagnose'),
    refetchInterval: 30_000,
    retry: false,
  })
}

export function DashboardHealth() {
  const { t } = useTranslation()
  const health = useDashboardHealth()
  return <div role="region" aria-label={t('admin:dashboardSystemHealth')}>
    {health.isError ? <a href="#dashboard-attention" className="rounded text-xs text-warning underline underline-offset-2 focus-visible:ring-2 focus-visible:ring-primary/40">{t('admin:dashboardSystemUnavailable')}</a>
      : health.data ? <HealthChips h={health.data} /> : <span role="status" className="text-xs text-muted-foreground">{t('admin:dashboardChecking')}</span>}
  </div>
}

/// "需要注意"面板。
///
/// 落地页的价值不是把所有数字摆出来，而是回答"我现在该做什么"。此前这里是一张
/// 三方对账漂移表——那是排障工具，不是日常动线，且没有漂移时整页空着。
/// 现在只列真正需要动手的项，全清时明确说"没有待办"而不是留一片空白。
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
        to: '/admin/ops',
        tone: 'destructive',
      })
    }
    if (h.outbox_pending >= 1_000) {
      items.push({
        key: 'outbox',
        summary: t('admin:dashboardBacklogIssue'),
        text: t('admin:attnOutbox', { n: h.outbox_pending }),
        to: '/admin/ops',
        tone: 'warning',
      })
    }
    if (h.cooling_keys > 0) {
      items.push({
        key: 'cooling',
        summary: t('admin:dashboardKeyIssue'),
        text: t('admin:attnCooling', { n: h.cooling_keys }),
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
        first: bad.find((c) => c.name)?.name ?? (bad.some((c) => c.channel_id > 0)
          ? t('admin:dashboardUnnamedChannel', { id: bad.find((c) => c.channel_id > 0)!.channel_id })
          : t('admin:dashboardUnassignedChannel')),
      }),
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
      to: '/admin/ops',
      tone: 'destructive',
    })
  }

  const queries = [channels, models, pools, drift, health, inventory]
  const incomplete = queries.some((query) => query.isError)
  const checking = queries.some((query) => query.isPending)
  items.sort((a, b) => Number(b.tone === 'destructive') - Number(a.tone === 'destructive'))

  return { items, h, incomplete, checking, retry: () => Promise.all(queries.filter((query) => query.isError).map((query) => query.refetch())) }
}

// 页头待办入口定位到实时区域内的同一份提醒，不另设重复栏目。
export function AttentionStatus({ days }: { days: number }) {
  const { t } = useTranslation()
  const { items, incomplete, checking } = useAttention(days)
  const urgent = items.some((item) => item.tone === 'destructive')
  const label = items.length > 0 ? t('admin:dashboardPending', { count: items.length })
    : incomplete ? t('admin:dashboardStatusIncomplete') : checking ? t('admin:dashboardChecking') : t('admin:dashboardNoPending')
  const first = items[0]
  const description = [incomplete ? t('admin:dashboardChecksIncomplete') : checking ? t('admin:dashboardChecking') : '', ...items.map((item) => item.text)].filter(Boolean).join('；')
  return <Tooltip content={description} className="min-w-0 max-w-full"><a href="#dashboard-attention" aria-label={first ? `${first.summary} · ${label}` : label} className={cn('inline-flex min-h-11 max-w-full items-center gap-1.5 rounded-full px-2.5 text-xs font-medium outline-none focus-visible:ring-2 focus-visible:ring-primary/40 md:min-h-8',
    urgent ? 'bg-destructive/10 text-destructive' : incomplete || items.length > 0 ? 'bg-warning/10 text-warning' : 'bg-muted text-muted-foreground')}>
    <span aria-hidden className={cn('h-1.5 w-1.5 shrink-0 rounded-full', urgent ? 'bg-destructive' : incomplete || items.length > 0 ? 'bg-warning' : checking ? 'bg-muted-foreground' : 'bg-success')} />
    <span className="min-w-0 max-w-36 truncate">{first?.summary ?? label}</span>
    {first && <span aria-hidden className="shrink-0 rounded bg-current/10 px-1.5 tabular-nums">{items.length}</span>}
    <ChevronRight aria-hidden size={12} className="shrink-0" />
  </a></Tooltip>
}

// 桌面横向四项，超过四项在原位展开；加载、检查失败和正常状态同样保留锚点。
export function AttentionPreview({ days }: { days: number }) {
  const { t } = useTranslation()
  const { items, incomplete, checking, retry } = useAttention(days)
  const [expanded, setExpanded] = useState(false)
  const listId = useId()
  useEffect(() => { setExpanded(false) }, [days])
  const visible = expanded ? items : items.slice(0, 4)

  return <section id="dashboard-attention" tabIndex={-1} aria-label={t('admin:dashboardPriorityActions')}
    className="min-w-0 scroll-mt-4 border-t border-border/60 px-3 py-1 text-xs outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-primary/40">
    <div className="flex min-w-0 flex-wrap items-center gap-x-3 gap-y-1 lg:flex-nowrap">
      <span className="inline-flex shrink-0 items-center gap-1 text-muted-foreground"><TriangleAlert aria-hidden size={13} />{t('admin:dashboardPriorityActions')}</span>
      {items.length > 0 && <div id={listId} className="grid min-w-0 basis-full grid-cols-1 gap-x-2 gap-y-1 sm:grid-cols-2 lg:flex-1 lg:basis-auto lg:grid-cols-4">
        {visible.map((item) => <Tooltip key={item.key} content={item.text} className="min-w-0">
          <Link to={item.to} search={item.search} aria-label={item.text}
            className="group flex min-h-11 w-full min-w-0 items-center gap-2 rounded-md px-2 py-0.5 outline-none hover:bg-muted/60 focus-visible:ring-2 focus-visible:ring-primary/40 lg:min-h-9">
            <span aria-hidden className={cn('h-1.5 w-1.5 shrink-0 rounded-full', item.tone === 'destructive' ? 'bg-destructive' : 'bg-warning')} />
            <span className="flex min-w-0 flex-1 flex-col gap-0.5">
              <span className={cn('truncate font-medium leading-4', item.tone === 'destructive' ? 'text-destructive' : 'text-foreground')}>{item.summary}</span>
              <span className="truncate text-[11px] leading-4 text-muted-foreground">{item.text}</span>
            </span>
            <ChevronRight aria-hidden size={12} className="shrink-0 text-muted-foreground group-hover:text-primary" />
          </Link>
        </Tooltip>)}
      </div>}
      {items.length > 4 && <button type="button" aria-expanded={expanded} aria-controls={listId} onClick={() => setExpanded((value) => !value)}
        className="ml-auto inline-flex min-h-8 shrink-0 items-center gap-1 rounded text-muted-foreground outline-none hover:text-primary focus-visible:ring-2 focus-visible:ring-primary/40">
        {expanded ? t('admin:dashboardShowLess') : t('admin:dashboardAllIssues', { count: items.length })}<ChevronRight aria-hidden size={12} className={cn('transition-transform', expanded ? '-rotate-90' : 'rotate-90')} />
      </button>}
      {items.length === 0 && !checking && !incomplete && <span className="inline-flex min-h-8 items-center gap-1.5 text-muted-foreground"><CheckCircle2 aria-hidden className="h-3.5 w-3.5 text-success" />{t('admin:attnClear')}</span>}
      {checking && <span role="status" className="inline-flex min-h-8 items-center text-muted-foreground">{t('admin:dashboardChecking')}</span>}
    </div>
    {incomplete && <div role="alert" className="flex min-w-0 items-center gap-2 py-1 text-muted-foreground">
      <TriangleAlert aria-hidden className="h-3.5 w-3.5 shrink-0 text-warning" />
      <span className="min-w-0 flex-1">{t('admin:dashboardChecksIncomplete')}</span>
      <Button size="xs" variant="outline" onClick={() => void retry()}>{t('common:retry')}</Button>
    </div>}
  </section>
}
