import { useQuery } from '@tanstack/react-query'
import dayjs from 'dayjs'
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
///
/// 渠道级"启用"只说明站长没有手动停它；一条 3 把 key 全在冷却的渠道照样是绿色
/// "启用"——这正是 new-api 用"自动禁用"状态、Sub2API 用账号状态视图去解决的盲区。
/// 汇总语义：全部 key 可用 → 静默（不加噪音）；部分不可用 → 黄，列出各态计数；
/// 一把可用的都没有 → 红"无可用 key"，这条渠道实际上已经下线。
export function KeyStateSummary({ keys, enabled }: { keys: ChannelKeyRow[]; enabled: boolean }) {
  const { t } = useTranslation()
  // 渠道级停用：站长手动关的，此时 key 状态无意义，直接说"已停用"（少一列"状态"）
  if (!enabled) {
    return <Badge variant="muted" dot>{t('common:disabled')}</Badge>
  }
  if (keys.length === 0) {
    return <Badge variant="destructive" dot>{t('admin:keyStateNoKeys')}</Badge>
  }
  const active = keys.filter((k) => k.status === 1).length
  if (active === keys.length) {
    return (
      <Badge variant="success" dot title={t('admin:keyStateAllActive', { n: keys.length })}>
        {t('admin:channelStateNormal')}
      </Badge>
    )
  }
  // 各非活跃态计数；冷却态附最近一把恢复的时间（"几分钟后自愈"与"要人工介入"是两种事）
  const counts = new Map<string, number>()
  for (const k of keys) {
    const state = KEY_STATE[k.status]
    if (state) counts.set(state, (counts.get(state) ?? 0) + 1)
  }
  const soonest = keys
    .filter((k) => k.cooldown_until !== null && (k.status === 2 || k.status === 3))
    .map((k) => dayjs(k.cooldown_until))
    .sort((a, b) => a.valueOf() - b.valueOf())[0]
  return (
    <div className="flex flex-wrap items-center gap-1 whitespace-nowrap">
      <Badge variant={active === 0 ? 'destructive' : 'warning'} dot>
        {active === 0
          ? t('admin:keyStateNoneActive')
          : t('admin:keyStateSomeActive', { active, total: keys.length })}
      </Badge>
      {[...counts.entries()].map(([state, n]) => (
        <Badge key={state} variant="muted" title={keys.find((k) => KEY_STATE[k.status] === state)?.last_error ?? ''}>
          {t(`admin:keyState_${state}`, { n })}
        </Badge>
      ))}
      {soonest && soonest.isAfter(dayjs()) && (
        <span className="text-xs text-muted-foreground">
          {t('admin:keyStateRecovers', { min: Math.max(1, Math.ceil(soonest.diff(dayjs(), 'minute', true))) })}
        </span>
      )}
    </div>
  )
}

/// 状态列的明细行：左列短标签说明是什么，右列是值。放在 `<dl>` 两列网格里，标签自然对齐。
/// 值默认常规字色，只有慢、失败、错误率高才上色——主状态看徽章，明细只在异常时抢眼。
export function StatusLine({ label, title, children }: { label: string; title?: string; children: ReactNode }) {
  return (
    <>
      <dt className="whitespace-nowrap text-muted-foreground">{label}</dt>
      <dd className="min-w-0 whitespace-nowrap" title={title}>{children}</dd>
    </>
  )
}

const stampOf = (when: dayjs.Dayjs) => (when.isSame(dayjs(), 'day') ? when.format('HH:mm') : when.format('MM-DD HH:mm'))

/// 订阅 OAuth 凭证（§11.38）的 access token 到期：最早到期的那把。过期不是故障——
/// 下次请求会刷新——所以只占一行明细，不进状态徽章。
export function CredentialExpiry({ keys }: { keys: ChannelKeyRow[] }) {
  const { t } = useTranslation()
  const soonest = keys
    .map((k) => k.credential_expires_at)
    .filter((v): v is number => typeof v === 'number')
    .sort((a, b) => a - b)[0]
  if (soonest === undefined) return null
  const when = dayjs.unix(soonest)
  return (
    <StatusLine label={t('admin:channelStatusCredential')} title={when.format('YYYY-MM-DD HH:mm')}>
      {when.isAfter(dayjs()) ? (
        <span className="tabular-nums">{t('admin:keyOAuthExpires', { when: stampOf(when) })}</span>
      ) : (
        <span className="text-muted-foreground">{t('admin:keyOAuthExpired')}</span>
      )}
    </StatusLine>
  )
}

/// 最近测试（new-api 渠道页"响应时间"列的对齐）：延迟 + 何时测的；失败给原因。
/// 时间只显示到分钟——这行回答"最近一次探测多快、还新鲜吗"，不是精确时间戳。
export function LastProbe({ probe }: { probe: ChannelProbe | null }) {
  const { t } = useTranslation()
  if (probe === null) {
    return <span className="text-muted-foreground">{t('admin:probeNever')}</span>
  }
  const when = dayjs(probe.at)
  // 探测的是 /models 这类轻量端点，>2s 已经算慢
  const result = probe.ok ? (
    <span className={cn('font-medium tabular-nums', probe.latency_ms >= 2_000 && 'text-warning')}>{probe.latency_ms} ms</span>
  ) : (
    <span className="font-medium text-destructive">
      {probe.http_status !== undefined ? `HTTP ${probe.http_status}` : (probe.error_code ?? t('logs:failed'))}
    </span>
  )
  return (
    <span className="inline-flex items-baseline gap-1.5">
      {result}
      <span className="text-muted-foreground tabular-nums" title={when.format('YYYY-MM-DD HH:mm')}>{stampOf(when)}</span>
    </span>
  )
}

/// 最近一次上游余额（§11.33）：金额随上游货币；余额 ≤ 0 标红——这条渠道已经打不动了。
/// 没查过不占位：不是每条渠道都有余额可查。
export function LastBalance({ balance }: { balance: ChannelBalance | null | undefined }) {
  const { i18n, t } = useTranslation()
  if (balance === null || balance === undefined) return null
  const when = dayjs(balance.at)
  const stamp = when.isSame(dayjs(), 'day') ? when.format('HH:mm') : when.format('MM-DD HH:mm')
  return (
    <span
      className="inline-flex items-center gap-1.5 whitespace-nowrap"
      title={t('admin:balanceProbe', { probe: balance.probe })}
    >
      <Badge variant={balance.balance_micro <= 0 ? 'destructive' : 'muted'}>
        {formatUpstreamBalance(balance.balance_micro, balance.currency, i18n.language)}
      </Badge>
      <span className="text-xs text-muted-foreground">{stamp}</span>
    </span>
  )
}

/// 近 24h 健康：错误率 + 请求数，取自 mv_channel_5min（与统计页渠道健康卡同源、同阈值）。
/// 整表一次查询、按 channel_id 分发到各行；CH 未启用时静默不显示，不报错。
export function useChannelHealth24h() {
  return useQuery({
    queryKey: qk.statsChannels(1, 100),
    queryFn: () => apiFetch<{ data: ChannelStatRow[] }>('/admin/stats/channels?days=1&limit=100'),
    retry: false,
    staleTime: 60_000,
  })
}

/// 点击打开该渠道的健康时间线抽屉（从几点开始坏、现在还坏吗），日志深链移到抽屉底部——
/// "看到 25% 错误率"之后的第一个问题是"什么时候开始的"，第二个才是"具体哪些请求"。
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
  const tone = { muted: 'text-muted-foreground', destructive: 'text-destructive', warning: 'text-warning', success: '' }[variant]
  return (
    <>
      <button
        type="button"
        className="-mx-1 inline-flex items-baseline gap-1.5 rounded px-1 whitespace-nowrap underline-offset-2 hover:bg-muted/60 hover:underline"
        title={idle ? t('admin:timelineOpen') : t('admin:health24hHint', { p95: stat.ttft_p95_ms })}
        onClick={() => setOpen(true)}
      >
        {idle ? (
          <span className="text-muted-foreground">{t('admin:health24hIdle')}</span>
        ) : (
          <>
            <span className="text-muted-foreground">{t('admin:health24hErrorRate')}</span>
            <span className={cn('font-medium tabular-nums', tone)}>{formatBp(stat.error_rate_bp, i18n.language)}</span>
            <span className="text-muted-foreground">
              · {t('admin:health24hRequests', { n: formatCount(stat.requests, i18n.language) })}
            </span>
          </>
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
  // 用量走分析库，要 billing.read；没有这项权限的管理员不发这次必 403 的请求，行内也就不显示用量
  const enabled = usePermission()('billing.read')
  return useQuery({
    enabled,
    queryKey: qk.statsChannelUsage(USAGE_DAYS),
    queryFn: () => apiFetch<{ data: ChannelUsageRow[] }>(`/admin/stats/channel-usage?days=${USAGE_DAYS}`),
    retry: false,
    staleTime: 60_000,
  })
}

/// 近 30 天用量胶囊：请求数 / token / 计费 / 上游成本各一块，悬停看输入 / 缓存 / 输出明细。
export function ChannelUsage({ usage }: { usage: ChannelUsageRow | undefined }) {
  const { t, i18n } = useTranslation()
  if (!usage || usage.requests === 0) return null
  const lang = i18n.language
  const total = usage.prompt_tokens + usage.completion_tokens
  const detail = t('admin:channelUsageDetail', {
    days: USAGE_DAYS,
    requests: formatCount(usage.requests, lang),
    prompt: formatCount(usage.prompt_tokens, lang),
    cached: formatCount(usage.cached_tokens, lang),
    completion: formatCount(usage.completion_tokens, lang),
    amount: formatMoneyAggregate(usage.amount_micro, lang),
    cost: formatMoneyAggregate(usage.upstream_cost_micro, lang),
  })
  const compact = new Intl.NumberFormat(lang, { notation: 'compact', maximumFractionDigits: 1 })
  const chip = 'inline-flex h-5 items-center rounded bg-muted px-1.5 text-[11px] text-muted-foreground tabular-nums'
  return (
    <span className="flex flex-wrap items-center gap-1 whitespace-nowrap" title={detail}>
      <span className={chip}>{t('admin:channelUsageChipRequests', { n: compact.format(usage.requests) })}</span>
      <span className={chip}>{t('admin:channelUsageChipTokens', { n: compact.format(total) })}</span>
      <span className={chip}>{t('admin:channelUsageChipAmount', { v: formatMoneyAggregate(usage.amount_micro, lang) })}</span>
    </span>
  )
}
