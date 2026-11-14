import { useQuery } from '@tanstack/react-query'
import { apiFetch } from '@/lib/api'
import { qk } from '@/lib/query-keys'
import type { ChannelSettings, LocalPeriod } from '../types'

export interface AuthorizationControls {
  code_format: 'code_state' | 'callback_url'
  access_token_prefix: string | null
  account_id_required: boolean
  import_profile?: NonNullable<ChannelSettings['extensions']>['client_profile'] | null
}
export interface AccountCapabilities {
  quota: boolean
  refresh: boolean
  authorization?: AuthorizationControls | null
  subscription: { quota_scope: 'session' | 'total'; window_secs: number | null; quota_windows?: number[] } | null
}
export interface UsageResponse {
  timezone: string
  token_usage?: { tokens: number; window_start: string | null; window_end: string | null }
  /// 本站记录的上游成本（微美元）；旧服务端没有这个字段
  cost_usage?: { cost_micro: number; window_start: string | null; window_end: string | null }
  quotas: Array<{ key_id: number; quota: {
    observed_at: number; allowed?: boolean | null; threshold_window?: string | null
    windows: Array<{ name: string; used_percent: number; resets_at: number | null; window_secs: number | null }>
    /// 按模型分的周窗口（订阅的 Fable 周限额）：只展示，不参与放行；旧观测没有这个字段
    scoped?: Array<{ model: string; used_percent: number; resets_at: number | null; window_secs: number | null }>
  } | null }>
}

export function useAccountCapabilities(provider: string) {
  const query = useQuery({
    queryKey: qk.channelProviders, staleTime: 300_000,
    queryFn: () => apiFetch<{ data: Array<{ id: string; default_base?: string | null; account: AccountCapabilities }> }>('/admin/channels/providers'),
  })
  const descriptor = query.data?.data.find((item) => item.id === provider)
  return { ...query, descriptor, capabilities: descriptor?.account }
}

/// 用量各按自己的周期查：列表行和抽屉用同样的周期时共用一份缓存。
export function useAccountUsage(channelId: number | undefined, periods: { tokens: LocalPeriod; cost: LocalPeriod }, enabled: boolean) {
  return useQuery({
    queryKey: qk.channelControlUsage(channelId, periods.tokens, periods.cost),
    enabled: channelId !== undefined && enabled,
    queryFn: () => apiFetch<UsageResponse>(`/admin/channels/${channelId}/usage?token_period=${periods.tokens}&cost_period=${periods.cost}`),
    refetchInterval: 30_000,
  })
}
