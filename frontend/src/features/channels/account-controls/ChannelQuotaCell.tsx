import type { ReactNode } from 'react'
import { useTranslation } from 'react-i18next'
import { formatUpstreamBalance } from '@/lib/money'
import type { AccountControl } from '../types'
import { useAccountCapabilities, useAccountUsage } from './api'
import type { UsageResponse } from './api'
import { CompactMeter, QuotaMeters, useResetSpan } from './QuotaMeters'

/// 列表行里的额度：订阅渠道是上游各窗口的用量条（配了百分比上限的窗口带一道刻度），
/// 设了成本 / Token 上限的渠道再加「已用 / 上限」的仪表。只有这两种情况才查询，与编辑抽屉共用同一份查询缓存。
/// 都没有、或还没有数据时显示 `fallback`（这一格别的内容，或一个占位符）。
export function ChannelQuotaCell({ channelId, provider, control, enabled = true, fallback = null }: {
  channelId: number
  provider: string
  /// 这条渠道保存的账号控制（列表行自带），上限从这里读，保存后立刻跟着变
  control?: Partial<AccountControl>
  /// 停用的渠道不查额度（后端也不再为它轮询），直接走 fallback
  enabled?: boolean
  fallback?: ReactNode
}) {
  const { capabilities } = useAccountCapabilities(provider)
  const cost = control?.local_cost ?? undefined, tokens = control?.local_tokens ?? undefined
  const capped = Boolean(cost || tokens)
  const usage = useAccountUsage(channelId, { tokens: tokens?.period ?? 'total', cost: cost?.period ?? 'total' },
    enabled && (Boolean(capabilities?.quota) || capped))
  // 列表里只是附加信息：响应缺字段或格式不对时不显示，绝不影响整张表
  const quota = enabled && capabilities?.quota && Array.isArray(usage.data?.quotas)
    ? usage.data.quotas.find((entry) => entry.quota)?.quota
    : undefined
  const meters = quota && Array.isArray(quota.windows) && quota.windows.length > 0
    ? <QuotaMeters quota={quota} limits={control?.quota_limits} compact /> : null
  const caps = enabled && capped && usage.data ? <LocalCapMeters control={control} usage={usage.data} /> : null
  if (!meters && !caps) return <>{fallback}</>
  return <>{meters}{caps}</>
}

/// 成本 / Token 上限的仪表：已用占上限的百分比，下面一行是「已用 / 上限」；到了上限换成何时恢复。
function LocalCapMeters({ control, usage }: { control?: Partial<AccountControl>; usage: UsageResponse }) {
  const { t, i18n } = useTranslation()
  const resetSpan = useResetSpan()
  const compact = (n: number) => new Intl.NumberFormat(i18n.language, { notation: 'compact', maximumFractionDigits: 1 }).format(n)
  const rows = [
    control?.local_cost && usage.cost_usage ? {
      key: 'cost', name: t('admin:channelCapCost'), period: control.local_cost.period ?? 'total',
      used: usage.cost_usage.cost_micro, cap: control.local_cost.cap_micro, ends: usage.cost_usage.window_end,
      // 和同一格里的上游余额一个写法：窄符号、两位小数，仪表下面那一行放得下
      text: (n: number) => formatUpstreamBalance(n, 'USD', i18n.language, true),
    } : null,
    control?.local_tokens && usage.token_usage ? {
      key: 'tokens', name: t('admin:channelCapTokens'), period: control.local_tokens.period ?? 'total',
      used: usage.token_usage.tokens, cap: control.local_tokens.cap, ends: usage.token_usage.window_end,
      text: compact,
    } : null,
  ].filter((row) => row !== null)
  if (rows.length === 0) return null
  return (
    <div data-slot="cap-meters" className="grid w-full max-w-[26rem] auto-cols-fr grid-flow-col gap-x-3 min-[1420px]:gap-x-4">
      {rows.map((row) => {
        const reached = row.used >= row.cap
        const percent = reached ? 100 : Math.max(0, Math.floor(row.used / row.cap * 100))
        const label = `${row.name} · ${t(`admin:channelPeriodShort_${row.period}`)}`
        const amount = `${row.text(row.used)} / ${row.text(row.cap)}`
        const reset = reached && row.ends ? resetSpan(Date.parse(row.ends) / 1000) : null
        return <CompactMeter key={row.key} slot="cap-meter" label={label} title={label}
          meterLabel={t('admin:channelCapMeterLabel', { name: label })} percent={percent}
          footerTitle={reset ? `${amount} · ${reset.full}` : amount}
          footer={<span className={reached ? 'truncate font-medium text-destructive' : 'truncate'}>
            {!reached ? amount : reset ? t('admin:channelCapReachedUntil', { span: reset.span }) : t('admin:channelCapReached')}
          </span>} />
      })}
    </div>
  )
}
