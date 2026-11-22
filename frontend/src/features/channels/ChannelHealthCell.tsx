import { useQuery } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import dayjs from 'dayjs'
import { ArrowUpRight, ChevronRight, Loader2, RefreshCw } from 'lucide-react'
import { useState, type ReactNode } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Tooltip } from '@/components/ui/tooltip'
import { ChannelTimelineDrawer } from '@/features/channels/ChannelTimelineDrawer'
import type { ChannelBalance, ChannelKeyRow, ChannelProbe } from '@/features/channels/types'
import type { ChannelHourlyBucket, ChannelRow as ChannelStatRow } from '@/features/stats/types'
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

/// 状态点 + 文字（轻量珐琅语义胶囊）。异常态支持直接点击进入对应处理区。
function StateDot({
  tone,
  title,
  onClick,
  children,
}: {
  tone: 'success' | 'warning' | 'destructive' | 'muted'
  title?: string
  onClick?: () => void
  children: ReactNode
}) {
  const color = { success: 'text-success', warning: 'text-warning', destructive: 'text-destructive', muted: 'text-muted-foreground' }[tone]
  const surface = {
    success: 'bg-success/10 ring-success/22',
    warning: 'bg-warning/12 ring-warning/28',
    destructive: 'bg-destructive/10 ring-destructive/22',
    muted: 'bg-muted/80 ring-border/75',
  }[tone]
  const cls = cn(
    'inline-flex h-[22px] shrink-0 items-center rounded-md px-2 text-xs leading-4 font-medium whitespace-nowrap ring-1 ring-inset',
    'before:mr-1.5 before:inline-block before:size-1.5 before:shrink-0 before:rounded-full before:bg-current before:content-[""]',
    color,
    surface,
    onClick && 'cursor-pointer transition-[filter,box-shadow] hover:brightness-95 dark:hover:brightness-110 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/40',
  )
  if (onClick) {
    return (
      <button type="button" data-slot="key-state-summary" className={cls} title={title} onClick={onClick}>
        {children}
      </button>
    )
  }
  return (
    <span data-slot="key-state-summary" className={cls} title={title}>
      {children}
    </span>
  )
}

export function KeyStateSummary({
  keys,
  enabled,
  onInspectKeys,
}: {
  keys: ChannelKeyRow[]
  enabled: boolean
  onInspectKeys?: () => void
}) {
  const { t } = useTranslation()
  const state = keyStateOf(keys, enabled)
  switch (state.kind) {
    case 'disabled':
      return <StateDot tone="muted">{t('common:disabled')}</StateDot>
    case 'noKeys':
      return <StateDot tone="destructive" onClick={onInspectKeys}>{t('admin:keyStateNoKeys')}</StateDot>
    case 'allActive':
      return (
        <StateDot tone="success" title={t('admin:keyStateAllActive', { n: state.total })}>
          {t('admin:channelStateNormal')}
        </StateDot>
      )
    case 'partial':
      return (
        <StateDot tone={state.active === 0 ? 'destructive' : 'warning'} onClick={onInspectKeys}>
          {state.active === 0
            ? t('admin:keyStateNoneActive')
            : t('admin:keyStateSomeActive', { active: state.active, total: state.total })}
        </StateDot>
      )
  }
}

/// 部分 key 不可用时的诊断条：各态计数、最近一把的恢复时间与首个错误原因；整条可直接点击去处置。
export function KeyStateDetail({
  keys,
  enabled,
  onInspect,
}: {
  keys: ChannelKeyRow[]
  enabled: boolean
  onInspect?: () => void
}) {
  const { t } = useTranslation()
  const state = keyStateOf(keys, enabled)
  if (state.kind !== 'partial') return null
  const critical = state.active === 0
  const firstError = [...state.titles.values()].map((s) => s.trim()).find(Boolean)
  const content = (
    <>
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
      {firstError && (
        <span className="max-w-[26rem] truncate font-mono text-[11px] text-muted-foreground/85" title={firstError}>
          · {firstError}
        </span>
      )}
      {onInspect && (
        <ChevronRight aria-hidden className="h-3 w-3 shrink-0 text-muted-foreground/60 transition-transform group-hover/alert:translate-x-0.5 group-hover/alert:text-foreground" />
      )}
    </>
  )
  const cls = cn(
    'group/alert inline-flex max-w-full flex-wrap items-center gap-x-2 gap-y-1 rounded-lg px-2.5 py-1 text-left text-xs ring-1 ring-inset transition-colors',
    critical
      ? 'bg-destructive/[0.06] ring-destructive/20 hover:bg-destructive/[0.1] dark:bg-destructive/[0.11]'
      : 'bg-warning/[0.08] ring-warning/22 hover:bg-warning/[0.14] dark:bg-warning/[0.12]',
    onInspect && 'cursor-pointer focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/40',
  )
  if (onInspect) {
    return (
      <button type="button" data-slot="key-state-detail" className={cls} onClick={onInspect}>
        {content}
      </button>
    )
  }
  return (
    <div data-slot="key-state-detail" className={cls}>
      {content}
    </div>
  )
}

const stampOf = (when: dayjs.Dayjs) => (when.isSame(dayjs(), 'day') ? when.format('HH:mm') : when.format('MM-DD HH:mm'))

/// 最近测试：延迟 + 测试时间；失败给错误码。直接点击即可重新发起一次测活。
export function LastProbe({
  probe,
  onProbe,
  probing = false,
}: {
  probe: ChannelProbe | null
  onProbe?: () => void
  probing?: boolean
}) {
  const { t } = useTranslation()
  const when = probe ? dayjs(probe.at) : null
  const failure = probe && !probe.ok
    ? (probe.http_status !== undefined ? `HTTP ${probe.http_status}` : (probe.error_code ?? t('logs:failed')))
    : null
  const plainHint = when
    ? t('admin:probeAt', { at: when.format('YYYY-MM-DD HH:mm') })
    : undefined
  const tooltipHint = when
    ? (
      <div className="w-[220px] max-w-full space-y-1.5 text-left whitespace-normal">
        <div className="flex items-center justify-between gap-3 text-[11.5px]">
          <span className="inline-flex min-w-0 items-center gap-1.5 font-medium">
            <span aria-hidden className={cn('size-1.5 shrink-0 rounded-full', probe?.ok ? 'bg-success' : 'bg-destructive')} />
            <span className={cn('truncate font-mono', probe?.ok ? 'text-foreground' : 'text-destructive')}>
              {probe?.ok ? `${probe.latency_ms} ms` : failure}
            </span>
          </span>
        </div>
        <p className="text-[11px] tabular-nums text-muted-foreground">
          {plainHint}
        </p>
        {onProbe && (
          <div className="border-t border-border/60 pt-1.5 text-[11px] text-muted-foreground">
            {t('admin:channelClickProbe')}
          </div>
        )}
      </div>
    )
    : (onProbe ? t('admin:channelClickProbe') : undefined)

  const inner = (
    <>
      {probing && <Loader2 aria-hidden className="h-3 w-3 shrink-0 animate-spin text-muted-foreground" />}
      {probe === null ? (
        <span className="text-xs whitespace-nowrap text-muted-foreground/75">{t('admin:probeNever')}</span>
      ) : (
        <>
          {probe.ok ? (
            <span className={cn('shrink-0 font-mono text-xs font-semibold tabular-nums', probe.latency_ms >= 2_000 ? 'text-warning' : 'text-foreground/85')}>
              {probe.latency_ms} ms
            </span>
          ) : (
            <span
              className="min-w-0 truncate rounded-md bg-destructive/10 px-1.5 py-0.5 font-mono text-[11px] font-medium text-destructive ring-1 ring-inset ring-destructive/20"
              title={failure ?? undefined}
            >
              {failure}
            </span>
          )}
          {when && (
            <span className="hidden text-[11px] text-muted-foreground/75 tabular-nums sm:inline">
              · {stampOf(when)}
            </span>
          )}
        </>
      )}
    </>
  )

  if (onProbe) {
    return (
      <Tooltip content={tooltipHint ?? ''}>
        <button
          type="button"
          data-slot="last-probe"
          disabled={probing}
          onClick={onProbe}
          className="-mx-1.5 inline-flex min-w-0 cursor-pointer items-center gap-1.5 rounded-md px-1.5 py-0.5 whitespace-nowrap outline-none transition-colors hover:bg-accent/80 focus-visible:ring-2 focus-visible:ring-primary/40 disabled:cursor-wait disabled:opacity-70"
        >
          {inner}
        </button>
      </Tooltip>
    )
  }

  return (
    <span
      data-slot="last-probe"
      className="inline-flex min-w-0 items-baseline gap-1.5 whitespace-nowrap"
      title={plainHint}
    >
      {inner}
    </span>
  )
}

/// 最近一次上游余额（§11.33）：横向仪表读数（金额 + 余额标签在左，查询时间 + 一键刷新在右）。
export function LastBalance({
  balance,
  onRefresh,
  refreshing = false,
}: {
  balance: ChannelBalance | null | undefined
  onRefresh?: () => void
  refreshing?: boolean
}) {
  const { i18n, t } = useTranslation()
  if (balance === null || balance === undefined) return null
  const when = dayjs(balance.at)
  const hasRatio = balance.total_micro !== null && balance.total_micro > 0 && balance.used_micro !== null
  const usedPct = hasRatio
    ? Math.min(100, Math.max(0, Math.round((balance.used_micro! / balance.total_micro!) * 100)))
    : null
  return (
    <div data-slot="channel-balance" className="flex min-w-0 flex-col gap-1" title={t('admin:balanceProbe', { probe: balance.probe })}>
      <div className="flex min-w-0 items-center justify-between gap-2">
        <span className="text-[11px] leading-4 font-medium text-muted-foreground/85">
          {t('admin:channelBalanceLabel')}
        </span>
        {onRefresh ? (
          <Tooltip content={t('admin:channelClickBalanceRefresh')}>
            <button
              type="button"
              disabled={refreshing}
              onClick={onRefresh}
              className="-mr-1.5 inline-flex cursor-pointer items-center gap-1 rounded-md px-1.5 py-0.5 text-[11px] whitespace-nowrap text-muted-foreground/85 tabular-nums outline-none transition-colors hover:bg-accent/80 hover:text-foreground focus-visible:ring-2 focus-visible:ring-primary/40 disabled:cursor-wait disabled:opacity-65"
            >
              <span>{t('admin:channelBalanceAt', { at: stampOf(when) })}</span>
              <RefreshCw aria-hidden className={cn('h-3 w-3 opacity-70', refreshing && 'animate-spin opacity-100')} />
            </button>
          </Tooltip>
        ) : (
          <span className="text-[11px] whitespace-nowrap text-muted-foreground/75 tabular-nums">
            {t('admin:channelBalanceAt', { at: stampOf(when) })}
          </span>
        )}
      </div>
      <div className="flex min-w-0 items-center gap-3">
        <span className={cn('shrink-0 text-[17px] leading-5 font-semibold tracking-[-0.015em] tabular-nums', balance.balance_micro <= 0 ? 'text-destructive' : 'text-foreground')}>
          {formatUpstreamBalance(balance.balance_micro, balance.currency, i18n.language, true)}
        </span>
        {usedPct !== null && (
          <div className="h-2 min-w-0 flex-1 overflow-hidden rounded-full bg-muted ring-1 ring-inset ring-border/75 dark:bg-white/[0.08] dark:ring-white/[0.08]">
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
    </div>
  )
}

/// 近 24h 健康：错误率 + 请求数 + 24 小时逐时成功率桶，取自 mv_channel_5min（与统计页渠道健康卡同源、同阈值）。
export function useChannelHealth24h() {
  return useQuery({
    queryKey: qk.statsChannels(1, 100),
    queryFn: () =>
      apiFetch<{ data: ChannelStatRow[]; hourly_start?: number }>('/admin/stats/channels?days=1&limit=100'),
    retry: false,
    staleTime: 60_000,
  })
}

const DEFAULT_SLOT_COUNT = 48
const DEFAULT_SLOT_SECS = 1_800

function defaultHourlySlots(hourlyStart?: number): ChannelHourlyBucket[] {
  const start =
    hourlyStart ??
    Math.floor(Date.now() / (DEFAULT_SLOT_SECS * 1000)) * DEFAULT_SLOT_SECS -
      (DEFAULT_SLOT_COUNT - 1) * DEFAULT_SLOT_SECS
  return Array.from({ length: DEFAULT_SLOT_COUNT }, (_, i) => ({
    ts: start + i * DEFAULT_SLOT_SECS,
    requests: 0,
    errors: 0,
  }))
}

function slotStateOf(slot: ChannelHourlyBucket): 'idle' | 'ok' | 'warn' | 'bad' {
  if (slot.requests <= 0) return 'idle'
  if (slot.errors <= 0) return 'ok'
  const errBp = Math.round((slot.errors / slot.requests) * 10_000)
  return errBp >= BAD_BP ? 'bad' : 'warn'
}

const SLOT_BAR_CLASS: Record<'idle' | 'ok' | 'warn' | 'bad', string> = {
  idle: 'bg-border/90 dark:bg-white/[0.11]',
  ok: 'bg-success/85 dark:bg-success/85',
  warn: 'bg-warning/90',
  bad: 'bg-destructive/90',
}

/// 近 24 小时分段成功率细柱（默认 48 根 30 分钟纤细圆角竖条，左→右为 24h 前→现在）：
/// 该时段全成功 = 绿，少量失败 (<5%) = 黄，高错误率 (≥5%) = 红，无请求 = 浅灰。
export function SuccessRate24hBars({
  hourly,
  hourlyStart,
  variant = 'compact',
}: {
  hourly?: ChannelHourlyBucket[]
  hourlyStart?: number
  variant?: 'compact' | 'expanded'
}) {
  const { t, i18n } = useTranslation()
  const [hoveredIdx, setHoveredIdx] = useState<number | null>(null)
  const slots = hourly && hourly.length > 0 ? hourly : defaultHourlySlots(hourlyStart)
  const stepSecs = slots.length >= 2 && slots[1].ts > slots[0].ts ? slots[1].ts - slots[0].ts : DEFAULT_SLOT_SECS
  const activeSlot = hoveredIdx !== null ? slots[hoveredIdx] : slots[slots.length - 1]

  const hoverCard = (() => {
    const start = dayjs.unix(activeSlot.ts)
    const end = dayjs.unix(activeSlot.ts + stepSecs)
    const sameDay = start.isSame(dayjs(), 'day')
    const range = sameDay
      ? `${start.format('HH:mm')} – ${end.format('HH:mm')}`
      : `${start.format('MM-DD HH:mm')} – ${end.format('HH:mm')}`
    const state = slotStateOf(activeSlot)
    const okCount = Math.max(0, activeSlot.requests - activeSlot.errors)
    const successBp = activeSlot.requests > 0 ? Math.max(0, 10_000 - Math.round((activeSlot.errors / activeSlot.requests) * 10_000)) : null
    const dotClass = {
      idle: 'bg-muted-foreground/45',
      ok: 'bg-success',
      warn: 'bg-warning',
      bad: 'bg-destructive',
    }[state]
    const statusTextClass = {
      idle: 'text-muted-foreground',
      ok: 'text-success',
      warn: 'text-warning',
      bad: 'text-destructive',
    }[state]
    return (
      <div className="w-[224px] max-w-full space-y-1.5 text-left whitespace-normal">
        <div className="flex items-center justify-between gap-2 border-b border-border/60 pb-1.5">
          <span className="font-mono text-[11px] font-medium text-foreground tabular-nums">{range}</span>
          <span className={cn('inline-flex items-center gap-1 text-[11px] font-semibold tabular-nums', statusTextClass)}>
            <span aria-hidden className={cn('size-1.5 shrink-0 rounded-full', dotClass)} />
            {state === 'idle'
              ? t('admin:health24hNoTraffic')
              : state === 'ok'
                ? `${formatBp(10_000, i18n.language)} · ${t('admin:health24hAllSuccess')}`
                : formatBp(successBp ?? 0, i18n.language)}
          </span>
        </div>
        {activeSlot.requests > 0 && (
          <div className="flex items-center justify-between gap-3 text-[11.5px] tabular-nums">
            <span className="text-muted-foreground">
              {t('admin:channelUsageRequests', { n: formatCount(activeSlot.requests, i18n.language) })}
            </span>
            <span className="font-medium text-foreground/90">
              {t('admin:health24hSlotSuccess', {
                ok: formatCount(okCount, i18n.language),
                err: formatCount(activeSlot.errors, i18n.language),
              })}
            </span>
          </div>
        )}
        <div className="border-t border-border/55 pt-1 text-[10.5px] text-muted-foreground/85">
          {t('admin:timelineOpen')}
        </div>
      </div>
    )
  })()

  return (
    <span className={cn('flex w-full min-w-0 flex-col gap-1', variant === 'compact' && 'max-w-[240px]')} title="">
      <Tooltip content={hoverCard} className="block w-full">
        <span
          data-slot="health-24h-bars"
          aria-label={t('admin:health24hBarsLabel')}
          onMouseLeave={() => setHoveredIdx(null)}
          className={cn(
            'flex w-full items-center',
            variant === 'expanded' ? 'h-[18px]' : 'h-3.5',
          )}
        >
          {slots.map((slot, idx) => {
            const state = slotStateOf(slot)
            const hovered = hoveredIdx === idx
            return (
              <span
                key={slot.ts}
                data-slot="health-24h-bar"
                data-state={state}
                onMouseEnter={() => setHoveredIdx(idx)}
                className="flex h-full min-w-0 flex-1 items-center justify-center"
              >
                <span
                  aria-hidden
                  className={cn(
                    'block h-full rounded-full transition-[transform,opacity,filter] duration-100',
                    variant === 'expanded' ? 'w-[4px]' : 'w-[3px]',
                    SLOT_BAR_CLASS[state],
                    hovered && 'scale-y-115 brightness-110',
                    hoveredIdx !== null && !hovered && 'opacity-65',
                  )}
                />
              </span>
            )
          })}
        </span>
      </Tooltip>
      {variant === 'expanded' && (
        <span className="flex items-center justify-between text-[10.5px] leading-3 text-muted-foreground/65 tabular-nums">
          <span>{t('admin:health24hAgo')}</span>
          <span>{t('admin:health24hNow')}</span>
        </span>
      )}
    </span>
  )
}

/// 近 24h 健康遥测块：整块区域均可点击打开健康时间线（无论点错误率、24h 绿色柱还是 TTFT，都符合直觉）。
export function Health24h({
  stat,
  hourlyStart,
  channel,
  layout = 'list',
}: {
  stat: ChannelStatRow | undefined
  hourlyStart?: number
  channel: { id: number; name: string; provider: string }
  layout?: 'list' | 'card'
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
  const successTone = { muted: 'text-muted-foreground', destructive: 'text-destructive', warning: 'text-warning', success: 'text-success' }[variant]
  const showLatency = !idle && (stat.ttft_p50_ms > 0 || stat.ttft_p95_ms > 0 || stat.failovers > 0)
  const successBp = idle ? null : Math.max(0, 10_000 - stat.error_rate_bp)

  if (layout === 'card') {
    return (
      <>
        <button
          type="button"
          className="group/health flex w-full min-w-0 cursor-pointer flex-col gap-2 rounded-lg text-left outline-none focus-visible:ring-2 focus-visible:ring-primary/40"
          title={idle ? t('admin:timelineOpen') : t('admin:health24hHint', { p95: stat.ttft_p95_ms })}
          onClick={() => setOpen(true)}
        >
          <span className="flex w-full min-w-0 items-baseline justify-between gap-2 tabular-nums">
            <span className="inline-flex items-center gap-1 text-[11px] leading-4 font-medium text-muted-foreground/85">
              <span>{t('admin:health24hSuccessRate')}</span>
              <ChevronRight
                aria-hidden
                className="h-3 w-3 shrink-0 text-muted-foreground/50 transition-[transform,color] group-hover/health:translate-x-0.5 group-hover/health:text-foreground"
              />
            </span>
            <span data-slot="health-24h" className="inline-flex min-w-0 items-baseline gap-1.5 whitespace-nowrap">
              {idle ? (
                <span className="text-xs text-muted-foreground/75">{t('admin:health24hIdle')}</span>
              ) : (
                <>
                  <span className={cn('text-[15px] leading-5 font-semibold tracking-[-0.015em]', successTone)}>
                    {formatBp(successBp ?? 0, i18n.language)}
                  </span>
                  <span className="text-[11.5px] text-muted-foreground">
                    · {t('admin:channelUsageRequests', { n: formatCount(stat.requests, i18n.language) })}
                  </span>
                </>
              )}
            </span>
          </span>
          <SuccessRate24hBars hourly={stat?.hourly} hourlyStart={hourlyStart} variant="expanded" />
          {showLatency && (
            <span data-slot="health-ttft" className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1 text-[11px] leading-4 text-muted-foreground tabular-nums">
              {stat.ttft_p50_ms > 0 && (
                <span className="whitespace-nowrap">
                  TTFT p50 <span className="font-medium text-foreground/80">{formatCount(stat.ttft_p50_ms, i18n.language)} ms</span>
                  {stat.ttft_p95_ms > 0 && (
                    <> · p95 <span className="font-medium text-foreground/80">{formatCount(stat.ttft_p95_ms, i18n.language)} ms</span></>
                  )}
                </span>
              )}
              {stat.failovers > 0 && (
                <span className="inline-flex shrink-0 items-center rounded-md bg-warning/12 px-1.5 py-px text-[10.5px] font-medium whitespace-nowrap text-warning ring-1 ring-inset ring-warning/25">
                  {t('admin:statFailovers')} {formatCount(stat.failovers, i18n.language)}
                </span>
              )}
            </span>
          )}
        </button>
        {open && <ChannelTimelineDrawer channel={channel} open onClose={() => setOpen(false)} />}
      </>
    )
  }

  return (
    <>
      <button
        type="button"
        className="group/health -mx-2 -my-1 flex w-[calc(100%+1rem)] min-w-0 cursor-pointer flex-col gap-1.5 rounded-lg px-2 py-1 text-left outline-none transition-colors hover:bg-accent/70 focus-visible:ring-2 focus-visible:ring-primary/40"
        title={idle ? t('admin:timelineOpen') : t('admin:health24hHint', { p95: stat.ttft_p95_ms })}
        onClick={() => setOpen(true)}
      >
        <span data-slot="health-24h" className="flex min-w-0 flex-col gap-0.5 tabular-nums">
          <span className="inline-flex items-center gap-1 text-[11px] leading-4 font-medium text-muted-foreground/85">
            <span>{t('admin:channelStatus24h')}</span>
            <ChevronRight
              aria-hidden
              className="h-3 w-3 shrink-0 text-muted-foreground/50 transition-[transform,color] group-hover/health:translate-x-0.5 group-hover/health:text-foreground"
            />
          </span>
          {' '}
          {idle ? (
            <span className="text-[13px] leading-5 text-muted-foreground/75">{t('admin:health24hIdle')}</span>
          ) : (
            <span className="flex min-w-0 items-baseline gap-1 truncate whitespace-nowrap">
              <span className={cn('text-[16px] leading-5 font-semibold tracking-[-0.015em] tabular-nums', tone)}>
                {formatBp(stat.error_rate_bp, i18n.language)}
              </span>
              <span className="text-xs text-muted-foreground">
                {' '}{t('admin:health24hErrorRate')} ·{' '}
                <span className="font-medium text-foreground/85">
                  {t('admin:channelUsageRequests', { n: formatCount(stat.requests, i18n.language) })}
                </span>
              </span>
            </span>
          )}
        </span>
        <SuccessRate24hBars hourly={stat?.hourly} hourlyStart={hourlyStart} variant="compact" />
        {showLatency && (
          <span data-slot="health-ttft" className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1 text-[11.5px] leading-4 text-muted-foreground tabular-nums">
            {stat.ttft_p50_ms > 0 && (
              <span className="whitespace-nowrap">
                TTFT p50 <span className="font-medium text-foreground/80">{formatCount(stat.ttft_p50_ms, i18n.language)} ms</span>
                {stat.ttft_p95_ms > 0 && (
                  <> · p95 <span className="font-medium text-foreground/80">{formatCount(stat.ttft_p95_ms, i18n.language)} ms</span></>
                )}
              </span>
            )}
            {stat.failovers > 0 && (
              <span className="inline-flex shrink-0 items-center rounded-md bg-warning/12 px-1.5 py-px text-[10.5px] font-medium whitespace-nowrap text-warning ring-1 ring-inset ring-warning/25">
                {t('admin:statFailovers')} {formatCount(stat.failovers, i18n.language)}
              </span>
            )}
          </span>
        )}
      </button>
      {open && <ChannelTimelineDrawer channel={channel} open onClose={() => setOpen(false)} />}
    </>
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

/// 近 30 天用量遥测块：悬停即见完整输入/缓存/输出与上游成本明细，点击直达该渠道的请求日志。
export function ChannelUsage({
  usage,
  channelId,
  settled = false,
}: {
  usage: ChannelUsageRow | undefined
  channelId?: number
  settled?: boolean
}) {
  const { t, i18n } = useTranslation()
  if (!usage || usage.requests === 0) {
    if (!settled) return null
    return (
      <div
        data-slot="channel-usage"
        className="flex flex-row items-baseline justify-between gap-2 md:flex-col md:items-end md:gap-0.5"
        title={t('admin:channelUsageEmpty')}
      >
        <span className="text-[11px] leading-4 font-medium text-muted-foreground/80">{t('admin:channelColUsage30')}</span>
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
  const upstreamCost = formatMoneyAggregate(usage.upstream_cost_micro, lang, false, true)
  const cachePct = usage.prompt_tokens > 0 && usage.cached_tokens > 0
    ? `${Math.round((usage.cached_tokens / usage.prompt_tokens) * 100)}%`
    : null
  const detail = t('admin:channelUsageDetail', {
    days: USAGE_DAYS,
    requests: formatCount(usage.requests, lang),
    prompt: formatCount(usage.prompt_tokens, lang),
    cached: formatCount(usage.cached_tokens, lang),
    completion: formatCount(usage.completion_tokens, lang),
    amount,
    cost: upstreamCost,
  })
  const detailCard = (
    <div className="w-60 space-y-2 text-left">
      <div className="flex items-baseline justify-between gap-3 border-b border-border/60 pb-1.5">
        <span className="text-[11px] font-medium text-muted-foreground">{t('admin:channelColUsage30')}</span>
        <span className="text-xs font-medium text-foreground tabular-nums">
          {t('admin:channelUsageRequests', { n: formatCount(usage.requests, lang) })}
        </span>
      </div>
      <div className="space-y-1 tabular-nums">
        <div className="flex items-baseline justify-between gap-3">
          <span className="text-muted-foreground">{t('admin:channelUsageBilled')}</span>
          <span className="font-semibold text-foreground">{amount}</span>
        </div>
        <div className="flex items-baseline justify-between gap-3">
          <span className="text-muted-foreground">{t('admin:channelUsageUpstreamCost')}</span>
          <span className="font-medium text-foreground/85">{upstreamCost}</span>
        </div>
      </div>
      <div className="space-y-1 border-t border-border/55 pt-1.5 text-[11.5px] tabular-nums">
        <div className="flex items-baseline justify-between gap-3">
          <span className="text-muted-foreground">{t('logs:inputShort')}</span>
          <span className="text-foreground/90">{formatCount(usage.prompt_tokens, lang)}</span>
        </div>
        {usage.cached_tokens > 0 && (
          <div className="flex items-baseline justify-between gap-3 pl-2.5 text-[11px]">
            <span className="text-muted-foreground/85">{t('logs:cacheRead')}</span>
            <span className="font-medium text-success">
              {formatCount(usage.cached_tokens, lang)}
              {cachePct && <span className="ml-1 text-muted-foreground font-normal">({cachePct})</span>}
            </span>
          </div>
        )}
        <div className="flex items-baseline justify-between gap-3">
          <span className="text-muted-foreground">{t('logs:outputShort')}</span>
          <span className="text-foreground/90">{formatCount(usage.completion_tokens, lang)}</span>
        </div>
      </div>
      <div className="border-t border-border/55 pt-1.5 text-[11px] text-muted-foreground/85">
        {t('admin:channelClickLogs')}
      </div>
    </div>
  )
  const inner = (
    <>
      <span className="inline-flex items-center gap-1 text-[11px] leading-4 font-medium whitespace-nowrap text-muted-foreground/85">
        {channelId !== undefined && (
          <ArrowUpRight
            aria-hidden
            className="h-3 w-3 shrink-0 text-muted-foreground/50 opacity-0 transition-[opacity,transform,color] group-hover/usage:translate-x-0.5 group-hover/usage:-translate-y-0.5 group-hover/usage:text-foreground group-hover/usage:opacity-100"
          />
        )}
        <span>{t('admin:channelColUsage30')}</span>
      </span>
      <span data-slot="channel-spend" className="text-[17px] leading-5 font-semibold tracking-[-0.015em] text-foreground tabular-nums">
        {amount}
      </span>
      <span className="max-w-full truncate text-[11.5px] leading-4 whitespace-nowrap text-muted-foreground tabular-nums">
        {t('admin:channelUsageRequests', { n: formatCount(usage.requests, lang) })} · {compact.format(total)} tokens
      </span>
    </>
  )

  if (channelId !== undefined) {
    return (
      <Tooltip content={detailCard} className="block w-full">
        <Link
          to="/admin/logs"
          search={{ channel_id: channelId }}
          data-slot="channel-usage"
          className="group/usage -mx-2 -my-1 flex w-[calc(100%+1rem)] min-w-0 flex-col items-start gap-0.5 rounded-lg px-2 py-1 outline-none transition-colors hover:bg-accent/70 focus-visible:ring-2 focus-visible:ring-primary/40 md:items-end"
        >
          {inner}
        </Link>
      </Tooltip>
    )
  }

  return (
    <div
      data-slot="channel-usage"
      className="flex min-w-0 flex-col items-start gap-0.5 md:items-end"
      title={detail}
    >
      {inner}
    </div>
  )
}
