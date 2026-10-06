import { useQuery } from '@tanstack/react-query'
import { apiFetch } from '@/lib/api'
import { qk } from '@/lib/query-keys'
import type { ChannelSettings } from '../types'

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
export type TokenPeriod = 'total' | 'day' | 'week'
export interface UsageResponse {
  timezone: string
  token_usage?: { tokens: number; window_start: string | null; window_end: string | null }
  quotas: Array<{ key_id: number; quota: {
    observed_at: number; allowed?: boolean | null; threshold_window?: string | null
    windows: Array<{ name: string; used_percent: number; resets_at: number | null; window_secs: number | null }>
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

export function useAccountUsage(channelId: number | undefined, period: TokenPeriod, enabled: boolean) {
  return useQuery({
    queryKey: qk.channelControlUsage(channelId, period),
    enabled: channelId !== undefined && enabled,
    queryFn: () => apiFetch<UsageResponse>(`/admin/channels/${channelId}/usage?token_period=${period}`),
    refetchInterval: 30_000,
  })
}
