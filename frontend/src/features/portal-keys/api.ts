import { apiFetch } from '@/lib/api'
import type { Page } from '@/lib/query-keys'

// `/api/me/keys` 的接口层：门户密钥页、试用台、日志页的密钥筛选、新手引导都读这一个接口，
// 行的类型只在这里写一份（后端 console portal/keys.rs 的 `keys`）。

/// available = 服务端存有加密副本、能代用户以这把令牌调用；not_saved = 旧令牌没存；unavailable = 站点没配主密钥。
export type KeyCopyStatus = 'available' | 'not_saved' | 'unavailable'

/// 近 7 天按天的 Token 数（门户密钥列表的折线）。
export interface KeyTokenTrend {
  days: string[]
  tokens: number[]
  timezone: string
}

export interface MyKey {
  id: number
  name: string
  key_prefix: string
  copy_status: KeyCopyStatus
  /// 1 启用 / 2 停用 / 3 过期。
  status: number
  used_micro: number
  /// 0 不限额 / 1 按额度。
  quota_mode: number
  quota_micro: number | null
  rpm_limit: number | null
  tpm_limit: number | null
  rpd_limit: number | null
  daily_token_limit: number | null
  max_concurrency: number | null
  /// null = 不限模型；否则精确匹配的模型名单。
  model_allowlist: string[] | null
  /// 这把 key 钉住的分组；null = 跟随用户分组。
  group_override: string | null
  /// 数据面来源 IP 白名单（地址 / CIDR）；null = 不限。
  ip_allowlist: string[] | null
  expires_at: string | null
  last_used_at: string | null
  created_at: string
  /// 累计消费与请求数（ClickHouse 聚合；没配 ClickHouse 时为 0）。
  amount_micro: number
  requests: number
  usage_trend: KeyTokenTrend | null
}

export interface MyKeysPage {
  data: MyKey[]
  total: number
  key_limits_supported?: boolean
}

/// 一页密钥；不传 page 取全部（日志页的密钥筛选要全量）。
export function fetchMyKeys(page?: Page): Promise<MyKeysPage> {
  return apiFetch<MyKeysPage>(page ? `/api/me/keys?limit=${page.limit}&offset=${page.offset}` : '/api/me/keys')
}
