import { useQuery } from '@tanstack/react-query'
import dayjs from 'dayjs'
import { ChevronRight } from 'lucide-react'
import { useState, type ReactNode } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { ChannelTimelineDrawer } from '@/features/channels/ChannelTimelineDrawer'
import type { ChannelBalance, ChannelKeyRow, ChannelProbe } from '@/features/channels/types'
import type { ChannelRow as ChannelStatRow } from '@/features/stats/types'
import { BAD_BP, WARN_BP } from '@/features/stats/types'
import { usePermission } from '@/hooks/use-auth'
import { apiFetch } from '@/lib/api'
import { formatBp, formatCount, formatMoneyAggregate, formatUpstreamBalance } from '@/lib/money'
import { qk } from '@/lib/query-keys'
import { cn } from '@/lib/utils'

/// channel_keys.status：1 active / 2 cooling / 3 rate_limited / 4 quota_exhausted / 5 banned / 6 invalid
const KEY_STATE: Record<number, string> = {
  2: 'cooling',
  3: 'rateLimited',
  4: 'quotaExhausted',
  5: 'banned',
  6: 'invalid',
}

/// 渠道行的"实际能不能打"：key 状态机汇总 + 近 24h 错误率。
type KeyStateView =
  | { kind: 'disabled' }
  | { kind: 'noKeys' }
  | { kind: 'allActive'; total: number }
  | { kind: 'partial'; active: number; total: number; counts: [string, number][]; titles: Map<string, string>; soonest?: dayjs.Dayjs }

function keyStateOf(keys: ChannelKeyRow[], enabled: boolean): KeyStateView {
  if (!enabled) return { kind: 'disabled' }
  if (keys.length === 0) return { kind: 'noKeys' }
  const active = keys.filter((k) => k.status === 1).length
  if (active === keys.length) return { kind: 'allActive', total: keys.length }
  const counts = new Map<string, number>()
  const titles = new Map<string, string>()
  for (const k of keys) {
    const state = KEY_STATE[k.status]
    if (!state) continue
    counts.set(state, (counts.get(state) ?? 0) + 1)
    if (!titles.has(state)) titles.set(state, k.last_error ?? '')
  }
  const soonest = keys
    .filter((k) => k.cooldown_until !== null && (k.status === 2 || k.status === 3))
    .map((k) => dayjs(k.cooldown_until))
    .sort((a, b) => a.valueOf() - b.valueOf())[0]
  return { kind: 'partial', active, total: keys.length, counts: [...counts.entries()], titles, soonest }
}

/// 渠道行的语义健康度：决定卡片左侧色条与边框微色（无可用 key / 未入池 = destructive，部分不可用或近 24h 高错 = warning）。
export function channelRowHealth(keys: ChannelKeyRow[], enabled: boolean, orphan: boolean, stat?: ChannelStatRow): 'normal' | 'warning' | 'destructive' | 'disabled' {
  if (!enabled) return 'disabled'
  if (orphan) return 'destructive'
  const state = keyStateOf(keys, enabled)
  if (state.kind === 'noKeys' || (state.kind === 'partial' && state.active === 0)) return 'destructive'
  if (state.kind === 'partial') return 'warning'
  if (stat && stat.requests > 0 && stat.error_rate_bp >= BAD_BP) return 'warning'
  return 'normal'
}

/// 状态点 + 文字（轻量珐琅语义胶囊）。
function StateDot({ tone, title, children }: { tone: 'success' | 'warning' | 'destructive' | 'muted'; title?: string; children: ReactNode }) {
  const color = { success: 'text-success', warning: 'text-warning', destructive: 'text-destructive', muted: 'text-muted-foreground' }[tone]
  const surface = {
    success: 'bg-success/10 ring-success/22',
    warning: 'bg-warning/12 ring-warning/28',
    destructive: 'bg-destructive/10 ring-destructive/22',
    muted: 'bg-muted/80 ring-border/75',
  }[tone]
  return (
    <span
      className={cn(
        'inline-flex h-[22px] shrink-0 items-center rounded-md px-2 text-xs leading-4 font-medium whitespace-nowrap ring-1 ring-inset',
        'before:mr-1.5 before:inline-block before:size-1.5 before:shrink-0 before:rounded-full before:bg-current before:content-[""]',
        color,
        surface,
      )}
      title={title}
    >
      {children}
    </span>
  )
}

export function KeyStateSummary({ keys, enabled }: { keys: ChannelKeyRow[]; enabled: boolean }) {
  const { t } = useTranslation()
  const state = keyStateOf(keys, enabled)
  switch (state.kind) {
    case 'disabled':
      return <StateDot tone="muted">{t('common:disabled')}</StateDot>
    case 'noKeys':
      return <StateDot tone="destructive">{t('admin:keyStateNoKeys')}</StateDot>
    case 'allActive':
      return (
        <StateDot tone="success" title={t('admin:keyStateAllActive', { n: state.total })}>
          {t('admin:channelStateNormal')}
        </StateDot>
      )
    case 'partial':
      return (
        <StateDot tone={state.active === 0 ? 'destructive' : 'warning'}>
          {state.active === 0
            ? t('admin:keyStateNoneActive')
            : t('admin:keyStateSomeActive', { active: state.active, total: state.total })}
        </StateDot>
      )
  }
}

/// 部分 key 不可用时的告警条：各态计数与最近一把的恢复时间；其余状态不占位。
export function KeyStateDetail({ keys, enabled }: { keys: ChannelKeyRow[]; enabled: boolean }) {
  const { t } = useTranslation()
  const state = keyStateOf(keys, enabled)
  if (state.kind !== 'partial') return null
  const critical = state.active === 0
  return (
    <div
      data-slot="key-state-detail"
      className={cn(
        'inline-flex max-w-full flex-wrap items-center gap-x-2 gap-y-1 rounded-lg px-2.5 py-1 text-xs ring-1 ring-inset',
        critical
          ? 'bg-destructive/[0.06] ring-destructive/20 dark:bg-destructive/[0.11]'
          : 'bg-warning/[0.08] ring-warning/22 dark:bg-warning/[0.12]',
      )}
    >
      <span className="sr-only">{t('admin:channelStatusKeys')}</span>
      {state.counts.map(([name, n]) => (
        <Badge
          key={name}
          variant={critical ? 'destructive' : 'warning'}
          className="min-h-[18px] rounded-[5px] px-1.5 py-0 text-[11px]"
          title={state.titles.get(name) ?? ''}
        >
          {t(`admin:keyState_${name}`, { n })}
        </Badge>
      ))}
      {state.soonest && state.soonest.isAfter(dayjs()) && (
        <span className="text-xs whitespace-nowrap text-muted-foreground tabular-nums">
          {t('admin:keyStateRecovers', { min: Math.max(1, Math.ceil(state.soonest.diff(dayjs(), 'minute', true))) })}
        </span>
      )}
    </div>
  )
}

const stampOf = (when: dayjs.Dayjs) => (when.isSame(dayjs(), 'day') ? when.format('HH:mm') : when.format('MM-DD HH:mm'))

/// 最近测试：延迟 + 测试时间；失败给错误码。
export function LastProbe({ probe }: { probe: ChannelProbe | null }) {
  const { t } = useTranslation()
  if (probe === null) {
    return <span data-slot="last-probe" className="text-xs whitespace-nowrap text-muted-foreground/75">{t('admin:probeNever')}</span>
  }
  const when = dayjs(probe.at)
  const failure = probe.http_status !== undefined ? `HTTP ${probe.http_status}` : (probe.error_code ?? t('logs:failed'))
  const result = probe.ok ? (
    <span className={cn('shrink-0 text-xs font-medium tabular-nums', probe.latency_ms >= 2_000 ? 'text-warning' : 'text-foreground/90')}>
      {probe.latency_ms} ms
    </span>
  ) : (
    <span className="min-w-0 truncate text-xs font-medium text-destructive" title={failure}>{failure}</span>
  )
  return (
    <span
      data-slot="last-probe"
      className="inline-flex min-w-0 items-baseline gap-1.5 whitespace-nowrap"
      title={t('admin:probeAt', { at: when.format('YYYY-MM-DD HH:mm') })}
    >
      {result}
      <span className="hidden text-[11px] text-muted-foreground/70 tabular-nums sm:inline">
        · {stampOf(when)}
      </span>
    </span>
  )
}

/// 最近一次上游余额（§11.33）：横向仪表读数（金额 + 余额标签在左，查询时间在右），与额度条视觉同源。
export function LastBalance({ balance }: { balance: ChannelBalance | null | undefined }) {
  const { i18n, t } = useTranslation()
  if (balance === null || balance === undefined) return null
  const when = dayjs(balance.at)
  const hasRatio = balance.total_micro !== null && balance.total_micro > 0 && balance.used_micro !== null
  const usedPct = hasRatio
    ? Math.min(100, Math.max(0, Math.round((balance.used_micro! / balance.total_micro!) * 100)))
    : null
  return (
    <div data-slot="channel-balance" className="flex min-w-0 flex-col gap-1.5" title={t('admin:balanceProbe', { probe: balance.probe })}>
      <div className="flex min-w-0 items-baseline justify-between gap-2">
        <span className="flex items-baseline gap-2 whitespace-nowrap">
          <span className={cn('text-[15px] leading-5 font-semibold tracking-[-0.01em] tabular-nums', balance.balance_micro <= 0 ? 'text-destructive' : 'text-foreground')}>
            {formatUpstreamBalance(balance.balance_micro, balance.currency, i18n.language, true)}
          </span>
          <span className="rounded bg-muted/85 px-1.5 py-px text-[10.5px] font-medium text-muted-foreground ring-1 ring-inset ring-border/60 dark:bg-white/[0.06]">
            {t('admin:channelBalanceLabel')}
          </span>
        </span>
        <span className="text-[11px] whitespace-nowrap text-muted-foreground/75 tabular-nums">
          {t('admin:channelBalanceAt', { at: stampOf(when) })}
        </span>
      </div>
      {usedPct !== null && (
        <div className="h-1.5 w-full overflow-hidden rounded-full bg-muted/90 ring-1 ring-inset ring-border/70 dark:bg-white/[0.07] dark:ring-white/[0.07]">
          <div
            className={cn(
              'h-full rounded-full transition-[width] duration-300',
              usedPct >= 90 ? 'bg-destructive' : usedPct >= 70 ? 'bg-warning' : 'bg-success',
            )}
            style={{ width: `${usedPct}%` }}
          />
        </div>
      )}
    </div>
  )
}

/// 近 24h 健康：错误率 + 请求数，取自 mv_channel_5min（与统计页渠道健康卡同源、同阈值）。
export function useChannelHealth24h() {
  return useQuery({
    queryKey: qk.statsChannels(1, 100),
    queryFn: () => apiFetch<{ data: ChannelStatRow[] }>('/admin/stats/channels?days=1&limit=100'),
    retry: false,
    staleTime: 60_000,
  })
}

/// 近 24h 健康遥测块：首行可点击打开健康时间线（错误率 + 请求数），次行直接露出首字延迟 p50/p95 与渠道切换次数。
export function Health24h({
  stat,
  channel,
}: {
  stat: ChannelStatRow | undefined
  channel: { id: number; name: string; provider: string }
}) {
  const { t, i18n } = useTranslation()
  const [open, setOpen] = useState(false)
  const idle = !stat || stat.requests === 0
  const variant = idle
    ? 'muted'
    : stat.error_rate_bp >= BAD_BP
      ? 'destructive'
      : stat.error_rate_bp >= WARN_BP
        ? 'warning'
        : 'success'
  const tone = { muted: 'text-muted-foreground', destructive: 'text-destructive', warning: 'text-warning', success: 'text-foreground' }[variant]
  const showLatency = !idle && (stat.ttft_p50_ms > 0 || stat.ttft_p95_ms > 0 || stat.failovers > 0)

  return (
    <div className="flex min-w-0 flex-col gap-0.5">
      <button
        type="button"
        data-slot="health-24h"
        className="group/health -mx-1.5 inline-flex max-w-full items-center gap-1 rounded-md px-1.5 py-0.5 text-left text-xs leading-5 whitespace-nowrap text-muted-foreground tabular-nums transition-colors hover:bg-accent/80 hover:text-foreground"
        title={idle ? t('admin:timelineOpen') : t('admin:health24hHint', { p95: stat.ttft_p95_ms })}
        onClick={() => setOpen(true)}
      >
        <span className="truncate">
          <span className="text-muted-foreground/75">{t('admin:channelStatus24h')}</span>{' '}
          {idle ? t('admin:health24hIdle') : (
            <>
              <span className={cn('font-semibold tabular-nums', tone)}>{formatBp(stat.error_rate_bp, i18n.language)}</span>{' '}
              {t('admin:health24hErrorRate')} · {t('admin:channelUsageRequests', { n: formatCount(stat.requests, i18n.language) })}
            </>
          )}
        </span>
        <ChevronRight
          aria-hidden
          className="h-3 w-3 shrink-0 text-muted-foreground/50 transition-[transform,opacity,color] group-hover/health:translate-x-0.5 group-hover/health:text-foreground group-hover/health:opacity-100"
        />
      </button>
      {showLatency && (
        <div data-slot="health-ttft" className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1 text-[11px] leading-4 text-muted-foreground/80 tabular-nums">
          {stat.ttft_p50_ms > 0 && (
            <span className="whitespace-nowrap">
              TTFT p50 {formatCount(stat.ttft_p50_ms, i18n.language)} ms
              {stat.ttft_p95_ms > 0 && <> · p95 {formatCount(stat.ttft_p95_ms, i18n.language)} ms</>}
            </span>
          )}
          {stat.failovers > 0 && (
            <span className="inline-flex shrink-0 items-center rounded bg-warning/12 px-1.5 py-px text-[10.5px] font-medium whitespace-nowrap text-warning ring-1 ring-inset ring-warning/25">
              {t('admin:statFailovers')} {formatCount(stat.failovers, i18n.language)}
            </span>
          )}
        </div>
      )}
      {open && <ChannelTimelineDrawer channel={channel} open onClose={() => setOpen(false)} />}
    </div>
  )
}

export interface ChannelUsageRow {
  channel_id: number
  requests: number
  /// 含缓存命中（OpenAI 口径），总量 = prompt + completion
  prompt_tokens: number
  cached_tokens: number
  completion_tokens: number
  amount_micro: number
  upstream_cost_micro: number
}

export const USAGE_DAYS = 30

/// 近 30 天每条渠道的 token 与金额（mv_cube_hour），整表一次查询、按 channel_id 分发。
export function useChannelUsage() {
  const enabled = usePermission()('billing.read')
  return useQuery({
    enabled,
    queryKey: qk.statsChannelUsage(USAGE_DAYS),
    queryFn: () => apiFetch<{ data: ChannelUsageRow[] }>(`/admin/stats/channel-usage?days=${USAGE_DAYS}`),
    retry: false,
    staleTime: 60_000,
  })
}

/// 近 30 天用量遥测块：金额打头，下面一行清晰呈现请求次数与 token 总量；明细（输入 / 缓存 / 输出、上游成本）在悬停提示里。
export function ChannelUsage({ usage, settled = false }: { usage: ChannelUsageRow | undefined; settled?: boolean }) {
  const { t, i18n } = useTranslation()
  if (!usage || usage.requests === 0) {
    if (!settled) return null
    return (
      <div
        data-slot="channel-usage"
        className="flex items-baseline justify-between gap-2 md:justify-end"
        title={t('admin:channelUsageEmpty')}
      >
        <span className="text-[11px] leading-5 text-muted-foreground/70">{t('admin:channelColUsage30')}</span>
        <span className="text-sm leading-5 text-muted-foreground/60">
          <span aria-hidden>—</span>
          <span className="sr-only">{t('admin:channelUsageEmpty')}</span>
        </span>
      </div>
    )
  }
  const lang = i18n.language
  const total = usage.prompt_tokens + usage.completion_tokens
  const compact = new Intl.NumberFormat(lang, { notation: 'compact', maximumFractionDigits: 1 })
  const amount = formatMoneyAggregate(usage.amount_micro, lang, false, true)
  const detail = t('admin:channelUsageDetail', {
    days: USAGE_DAYS,
    requests: formatCount(usage.requests, lang),
    prompt: formatCount(usage.prompt_tokens, lang),
    cached: formatCount(usage.cached_tokens, lang),
    completion: formatCount(usage.completion_tokens, lang),
    amount,
    cost: formatMoneyAggregate(usage.upstream_cost_micro, lang, false, true),
  })
  return (
    <div
      data-slot="channel-usage"
      className="flex min-w-0 flex-row items-baseline justify-between gap-2 md:flex-col md:items-end md:gap-0.5"
      title={detail}
    >
      <div className="flex items-baseline gap-1.5 whitespace-nowrap">
        <span className="text-[11px] text-muted-foreground/75">{t('admin:channelColUsage30')}</span>
        <span data-slot="channel-spend" className="text-[15px] leading-5 font-semibold tracking-[-0.01em] text-foreground tabular-nums">
          {amount}
        </span>
      </div>
      <span className="max-w-full truncate text-xs leading-5 whitespace-nowrap text-muted-foreground tabular-nums">
        {t('admin:channelUsageRequests', { n: formatCount(usage.requests, lang) })} · {compact.format(total)} tokens
      </span>
    </div>
  )
}
