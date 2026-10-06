import { useAccountCapabilities, useAccountUsage } from './api'
import { QuotaMeters } from './QuotaMeters'

/// 列表行里的订阅额度条：只有声明了额度能力的协议（订阅渠道）才查询和显示，
/// 与编辑抽屉共用同一份查询缓存。还没有观测数据时不占位。
export function ChannelQuotaCell({ channelId, provider }: { channelId: number; provider: string }) {
  const { capabilities } = useAccountCapabilities(provider)
  const usage = useAccountUsage(channelId, 'total', Boolean(capabilities?.quota))
  // 列表里只是附加信息：响应缺字段或格式不对时不显示，绝不影响整张表
  const quota = Array.isArray(usage.data?.quotas)
    ? usage.data.quotas.find((entry) => entry.quota)?.quota
    : undefined
  if (!capabilities?.quota || !quota || !Array.isArray(quota.windows) || quota.windows.length === 0) return null
  return (
    <div className="mt-1 w-full">
      <QuotaMeters quota={quota} compact />
    </div>
  )
}
