/// 出口绑定（与后端 `okapi_store::egress::Binding` 一致，IMPLEMENTATION §11.41）。
/// `inherit` = 跟随全局默认出口，只对渠道有意义；全局默认本身不能是 inherit。
export type EgressBinding =
  | { mode: 'inherit' }
  | { mode: 'direct' }
  | { mode: 'proxy'; proxy_id: number }
  | { mode: 'group'; group_code: string }

export type EgressMode = EgressBinding['mode']

export const INHERIT: EgressBinding = { mode: 'inherit' }
export const DIRECT: EgressBinding = { mode: 'direct' }

/// GET /admin/proxies 的一行（只有掩码地址，没有密码）。
export interface ProxyRow {
  id: number
  name: string
  owner_id: number | null
  scheme: string
  host: string
  port: number
  username: string | null
  /// 1 启用 / 2 手动停用；熔断不改它，看 `cooling`。
  status: number
  /// 固定分配组里最多分给几把 key（一个 IP 挂几个账号）；null = 不限。
  max_keys: number | null
  /// 经它同时在途的上游请求上限（跨 key、跨副本）；null = 不限。
  max_concurrency: number | null
  failed_count: number
  cooldown_until: string | null
  /// 连续连接失败进入的被动熔断（冷却到期自动半开放行）。
  cooling: boolean
  last_error: string | null
  exit_ip: string | null
  exit_country: string | null
  latency_ms: number | null
  checked_at: string | null
  /// 最近一次发现出口 IP 变化：变化前的 IP 与发现时刻。
  previous_exit_ip: string | null
  exit_ip_changed_at: string | null
  note: string | null
  url_masked: string
  /// 直接绑定它的渠道数。
  channel_count: number
  /// 固定分配到它的 key 数。
  assigned_keys: number
  groups: string[]
  is_default: boolean
}

export interface ProxyGroupMemberRow {
  proxy_id: number
  name: string
  status: number
  cooling: boolean
  priority: number
  weight: number
  max_keys: number | null
  /// 本组里分到该代理的 key 数（固定分配组才有意义）。
  assigned_keys: number
}

export type ProxyGroupMode = 'pinned' | 'rotate'

export interface ProxyGroupRow {
  code: string
  name: string
  mode: ProxyGroupMode
  owner_id: number | null
  description: string | null
  members: ProxyGroupMemberRow[]
  /// 直接绑定该组的渠道数（不含经全局默认继承的）。
  channel_count: number
  /// 有效出口是该组却没分到代理的 key 数（容量满 / 组里没有可用成员）。
  unassigned_keys: number
  is_default: boolean
}

/// 组成员（写入形态）。
export interface ProxyGroupMember {
  proxy_id: number
  priority: number
  weight: number
}

/// 写操作顺带的固定分配对账结果（全站口径）。
export interface ReconcileReport {
  assigned: number
  released: number
  unassigned: number
}

/// 测试结果（POST /admin/proxies/{id}/test 与 /admin/proxies/test）。
export interface ProbeResult {
  ok: boolean
  target: string
  status?: number
  latency_ms?: number
  exit_ip?: string | null
  country?: string | null
  error_code?: string
  error?: string
  /// 这次测试发现出口 IP 与上次不同。
  exit_ip_changed?: { previous: string; current: string }
}

/// POST /admin/proxies/import 的回执。
export interface ImportResult {
  created: { line: number; id: number; name: string; url_masked: string }[]
  skipped: { line: number; reason: 'invalid' | 'duplicate' }[]
  assignment: ReconcileReport | null
}

/// `settings.egress_probe_policy`（后台探测）。
export interface ProbePolicy {
  enabled: boolean
  interval_secs: number
  target: string | null
  concurrency: number
}

export const DEFAULT_PROBE_POLICY: ProbePolicy = {
  enabled: true,
  interval_secs: 600,
  target: null,
  concurrency: 4,
}

/// 7 天内发现过出口 IP 变化。
export function exitIpRecentlyChanged(proxy: Pick<ProxyRow, 'exit_ip_changed_at'>): boolean {
  if (proxy.exit_ip_changed_at === null) return false
  return Date.now() - new Date(proxy.exit_ip_changed_at).getTime() < 7 * 24 * 3600 * 1000
}

/// GET /admin/proxy-groups/{code}/assignments 的一行。
export interface AssignmentRow {
  key_id: number
  channel_id: number
  channel_name: string
  proxy_id: number | null
}

export const GROUP_MODES: ProxyGroupMode[] = ['pinned', 'rotate']

export const GROUP_MODE_LABEL = {
  pinned: 'admin:proxyGroupPinned',
  rotate: 'admin:proxyGroupRotate',
} as const

export const GROUP_MODE_HINT = {
  pinned: 'admin:proxyGroupPinnedHint',
  rotate: 'admin:proxyGroupRotateHint',
} as const
