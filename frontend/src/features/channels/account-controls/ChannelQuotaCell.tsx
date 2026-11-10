import type { ReactNode } from 'react'
import { useAccountCapabilities, useAccountUsage } from './api'
import { QuotaMeters } from './QuotaMeters'

/// 列表行里的订阅额度条：只有声明了额度能力的协议（订阅渠道）才查询和显示，
/// 与编辑抽屉共用同一份查询缓存。还没有观测数据时显示 `fallback`（这一格别的内容，或一个占位符）。
export function ChannelQuotaCell({ channelId, provider, enabled = true, fallback = null }: {
  channelId: number
  provider: string
  /// 停用的渠道不查额度（后端也不再为它轮询），直接走 fallback
  enabled?: boolean
  fallback?: ReactNode
}) {
  const { capabilities } = useAccountCapabilities(provider)
  const usage = useAccountUsage(channelId, 'total', enabled && Boolean(capabilities?.quota))
  // 列表里只是附加信息：响应缺字段或格式不对时不显示，绝不影响整张表
  const quota = enabled && Array.isArray(usage.data?.quotas)
    ? usage.data.quotas.find((entry) => entry.quota)?.quota
    : undefined
  if (!capabilities?.quota || !quota || !Array.isArray(quota.windows) || quota.windows.length === 0) return <>{fallback}</>
  return <QuotaMeters quota={quota} compact />
}
