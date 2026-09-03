import { useQuery } from '@tanstack/react-query'
import dayjs from 'dayjs'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { ChannelTimelineDrawer } from '@/features/channels/ChannelTimelineDrawer'
import type { ChannelKeyRow, ChannelProbe } from '@/features/channels/types'
import type { ChannelRow as ChannelStatRow } from '@/features/stats/types'
import { BAD_BP, WARN_BP } from '@/features/stats/types'
import { apiFetch } from '@/lib/api'
import { formatBp, formatCount } from '@/lib/money'
import { qk } from '@/lib/query-keys'

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
    return <Badge variant="muted">{t('common:disabled')}</Badge>
  }
  if (keys.length === 0) {
    return <Badge variant="destructive">{t('admin:keyStateNoKeys')}</Badge>
  }
  const active = keys.filter((k) => k.status === 1).length
  if (active === keys.length) {
    return (
      <span className="inline-flex items-center gap-1.5 whitespace-nowrap">
        <Badge variant="success">{t('common:enabled')}</Badge>
        <span className="text-xs text-muted-foreground">
          {t('admin:keyStateAllActive', { n: keys.length })}
        </span>
      </span>
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
      <Badge variant={active === 0 ? 'destructive' : 'warning'}>
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

/// 最近测活（new-api 渠道页"响应时间"列的对齐）：ms + 何时测的；失败给原因。
/// 时间只显示到分钟——这列回答"最近一次探测多快、还新鲜吗"，不是精确时间戳。
export function LastProbe({ probe }: { probe: ChannelProbe | null }) {
  const { t } = useTranslation()
  if (probe === null) {
    return <span className="text-xs text-muted-foreground">{t('admin:probeNever')}</span>
  }
  const when = dayjs(probe.at)
  const stamp = when.isSame(dayjs(), 'day') ? when.format('HH:mm') : when.format('MM-DD HH:mm')
  if (probe.ok) {
    // 探测的是 /models 这类轻量端点，>2s 已经算慢
    const variant = probe.latency_ms >= 2_000 ? 'warning' : 'success'
    return (
      <span className="inline-flex items-center gap-1.5 whitespace-nowrap">
        <Badge variant={variant}>{probe.latency_ms} ms</Badge>
        <span className="text-xs text-muted-foreground">{stamp}</span>
      </span>
    )
  }
  return (
    <span className="inline-flex items-center gap-1.5 whitespace-nowrap">
      <Badge variant="destructive">
        {probe.http_status !== undefined ? `HTTP ${probe.http_status}` : (probe.error_code ?? t('logs:failed'))}
      </Badge>
      <span className="text-xs text-muted-foreground">{stamp}</span>
    </span>
  )
}

/// 近 24h 健康：错误率 + 请求数，取自 mv_channel_5min（与统计页渠道健康卡同源、同阈值）。
/// 整表一次查询、按 channel_id 分发到各行；CH 未启用时静默不显示，不报错。
export function useChannelHealth24h() {
  return useQuery({
    queryKey: qk.statsChannels(1),
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
  return (
    <>
      <button
        type="button"
        className="inline-flex items-center gap-1.5 rounded-md whitespace-nowrap hover:bg-muted/60"
        title={idle ? t('admin:timelineOpen') : t('admin:health24hHint', { p95: stat.ttft_p95_ms })}
        onClick={() => setOpen(true)}
      >
        {idle ? (
          <span className="px-1 text-xs text-muted-foreground">—</span>
        ) : (
          <>
            <Badge variant={variant}>{formatBp(stat.error_rate_bp, i18n.language)}</Badge>
            <span className="text-xs text-muted-foreground">
              {t('admin:health24hRequests', { n: formatCount(stat.requests, i18n.language) })}
            </span>
          </>
        )}
      </button>
      {open && <ChannelTimelineDrawer channel={channel} open onClose={() => setOpen(false)} />}
    </>
  )
}
