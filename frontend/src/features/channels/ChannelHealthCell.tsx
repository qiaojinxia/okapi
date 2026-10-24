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
/// 汇总语义：全部 key 可用 → 绿点「正常」；部分不可用 → 黄点，下面列各态计数；
/// 一把可用的都没有 → 红点"无可用 key"，这条渠道实际上已经下线。
type KeyStateView =
  | { kind: 'disabled' }
  | { kind: 'noKeys' }
  | { kind: 'allActive'; total: number }
  | { kind: 'partial'; active: number; total: number; counts: [string, number][]; titles: Map<string, string>; soonest?: dayjs.Dayjs }

function keyStateOf(keys: ChannelKeyRow[], enabled: boolean): KeyStateView {
  // 渠道级停用：站长手动关的，此时 key 状态无意义，直接说"已停用"（少一列"状态"）
  if (!enabled) return { kind: 'disabled' }
  if (keys.length === 0) return { kind: 'noKeys' }
  const active = keys.filter((k) => k.status === 1).length
  if (active === keys.length) return { kind: 'allActive', total: keys.length }
  // 各非活跃态计数；冷却态附最近一把恢复的时间（"几分钟后自愈"与"要人工介入"是两种事）
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

/// 状态点 + 文字（没有底色胶囊）：一眼扫过去只看点的颜色，细节写在后面。
function StateDot({ tone, title, children }: { tone: 'success' | 'warning' | 'destructive' | 'muted'; title?: string; children: ReactNode }) {
  const color = { success: 'text-success', warning: 'text-warning', destructive: 'text-destructive', muted: 'text-muted-foreground' }[tone]
  // 点用伪元素画在文字前面：整块仍是一个行内元素，基线与右边的测试延迟对得上
  return (
    <span
      className={cn(
        'font-medium whitespace-nowrap before:mr-1.5 before:inline-block before:size-1.5 before:rounded-full before:bg-current before:align-middle',
        color,
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

/// 状态点后面的补充：部分 key 不可用时的各态计数与最近一把的恢复时间；其余状态没有补充。
export function KeyStateDetail({ keys, enabled }: { keys: ChannelKeyRow[]; enabled: boolean }) {
  const { t } = useTranslation()
  const state = keyStateOf(keys, enabled)
  if (state.kind !== 'partial') return null
  return (
    <>
      {state.counts.map(([name, n]) => (
        <Badge key={name} variant="muted" title={state.titles.get(name) ?? ''}>
          {t(`admin:keyState_${name}`, { n })}
        </Badge>
      ))}
      {state.soonest && state.soonest.isAfter(dayjs()) && (
        <span className="text-xs text-muted-foreground">
          {t('admin:keyStateRecovers', { min: Math.max(1, Math.ceil(state.soonest.diff(dayjs(), 'minute', true))) })}
        </span>
      )}
    </>
  )
}

/// 状态列的明细行：左列短标签说明是什么，右列是值。放在 `<dl>` 两列网格里，标签自然对齐。
/// 值默认常规字色，只有慢、失败、错误率高才上色——主状态看状态点，明细只在异常时抢眼。
export function StatusLine({ label, title, children }: { label: string; title?: string; children: ReactNode }) {
  return (
    <>
      <dt className="whitespace-nowrap text-muted-foreground/80">{label}</dt>
      <dd className="min-w-0 whitespace-nowrap" title={title}>{children}</dd>
    </>
  )
}

const stampOf = (when: dayjs.Dayjs) => (when.isSame(dayjs(), 'day') ? when.format('HH:mm') : when.format('MM-DD HH:mm'))

/// 订阅 OAuth 凭证（§11.38）的 access token 到期：最早到期的那把。过期不是故障——
/// 下次请求会刷新——所以只占一行明细，不进状态点。
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
    <span className={cn('font-semibold tabular-nums', probe.latency_ms >= 2_000 && 'text-warning')}>{probe.latency_ms} ms</span>
  ) : (
    <span className="font-semibold text-destructive">
      {probe.http_status !== undefined ? `HTTP ${probe.http_status}` : (probe.error_code ?? t('logs:failed'))}
    </span>
  )
  return (
    <span className="inline-flex items-baseline gap-1">
      {result}
      <span className="text-muted-foreground/70 tabular-nums" title={when.format('YYYY-MM-DD HH:mm')}>{stampOf(when)}</span>
    </span>
  )
}

/// 最近一次上游余额（§11.33）：金额随上游货币；余额 ≤ 0 标红——这条渠道已经打不动了。
/// 没查过不占位：不是每条渠道都有余额可查。
export function LastBalance({ balance }: { balance: ChannelBalance | null | undefined }) {
  const { i18n, t } = useTranslation()
  if (balance === null || balance === undefined) return null
  const when = dayjs(balance.at)
  return (
    <span
      className="inline-flex items-baseline gap-1.5 whitespace-nowrap"
      title={t('admin:balanceProbe', { probe: balance.probe })}
    >
      <span className="text-muted-foreground/80">{t('admin:channelBalanceLabel')}</span>
      <span className={cn('font-medium tabular-nums', balance.balance_micro <= 0 ? 'text-destructive' : 'text-foreground')}>
        {formatUpstreamBalance(balance.balance_micro, balance.currency, i18n.language)}
      </span>
      <span className="text-muted-foreground/70 tabular-nums">{stampOf(when)}</span>
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
        className="-mx-1 rounded px-1 whitespace-nowrap underline-offset-2 hover:bg-muted/60 hover:underline"
        title={idle ? t('admin:timelineOpen') : t('admin:health24hHint', { p95: stat.ttft_p95_ms })}
        onClick={() => setOpen(true)}
      >
        {idle ? (
          <span className="text-muted-foreground">{t('admin:health24hIdle')}</span>
        ) : (
          // 一整句「0.0% 错误 · 1 次」：错误率着色，其余同字色；真空格而不是 flex 间距，读屏与复制都是通顺的一句
          <>
            <span className={cn('font-medium tabular-nums', tone)}>{formatBp(stat.error_rate_bp, i18n.language)}</span>{' '}
            {t('admin:health24hErrorRate')} · {t('admin:channelUsageRequests', { n: formatCount(stat.requests, i18n.language) })}
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

/// 近 30 天用量一行：请求数 · token · 计费金额（金额是这行的重点，用前景色），
/// 悬停看输入 / 缓存 / 输出明细与上游成本。
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
  return (
    <span className="whitespace-nowrap text-muted-foreground tabular-nums" title={detail}>
      {t('admin:channelUsageRequests', { n: formatCount(usage.requests, lang) })}
      {' · '}
      {t('admin:channelUsageTokens', { n: compact.format(total) })}
      {' · '}
      <span className="font-medium text-foreground">{formatMoneyAggregate(usage.amount_micro, lang)}</span>
    </span>
  )
}
