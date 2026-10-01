import { useQuery } from '@tanstack/react-query'
import { isAvailable } from '@/features/public-pricing/catalog-data'
import type { PricingModel } from '@/features/public-pricing/types'
import { apiFetch } from '@/lib/api'
import { qk } from '@/lib/query-keys'

/// `/api/me/keys` 一行里试用台用到的字段。
interface KeyRow {
  id: number
  name: string
  key_prefix: string
  /// available = 服务端存有加密副本、能代用户以这把令牌调用；not_saved = 旧令牌没存；unavailable = 站点没配主密钥。
  copy_status: string
  /// 1 启用 / 2 停用 / 3 过期。
  status: number
  model_allowlist: unknown
  group_override: string | null
  expires_at: string | null
  quota_mode: number
  quota_micro: number | null
  used_micro: number
}

/// 为什么某把密钥当下不能选（null = 可选）。
export type KeyBlock = 'disabled' | 'expired' | 'not_saved' | 'unavailable'

/// 试用台里的一个"使用的密钥"候选：除了能不能选，还带上决定"这把能调哪些模型"的两项限制。
export interface KeyChoice {
  id: number
  name: string
  prefix: string
  /// 令牌自己钉的价目分组；null = 跟随账号分组。
  group: string | null
  /// 模型白名单；null = 不限（与网关一致：数组内精确匹配，空数组 = 一个都不让调）。
  allowlist: string[] | null
  /// 独立额度模式下的剩余额度（micro-USD）；共享钱包 / 不限额为 null。
  quotaLeftMicro: number | null
  blocked: KeyBlock | null
}

export function toChoice(row: KeyRow, now = Date.now()): KeyChoice {
  const expired = row.status === 3 || (row.expires_at !== null && Date.parse(row.expires_at) <= now)
  const blocked: KeyBlock | null =
    row.status === 2 ? 'disabled'
    : expired ? 'expired'
    : row.copy_status === 'not_saved' ? 'not_saved'
    : row.copy_status !== 'available' ? 'unavailable'
    : null
  return {
    id: row.id,
    name: row.name,
    prefix: row.key_prefix,
    group: row.group_override !== null && row.group_override !== '' ? row.group_override : null,
    allowlist: Array.isArray(row.model_allowlist) ? row.model_allowlist.filter((m): m is string => typeof m === 'string') : null,
    quotaLeftMicro: row.quota_mode === 1 && row.quota_micro !== null ? Math.max(row.quota_micro - row.used_micro, 0) : null,
    blocked,
  }
}

/// 本用户的全部密钥（上限 100 把，与指南页同口径取第一页）。失败时返回空：试用台退回"只用登录会话"。
export function usePlaygroundKeys() {
  const query = useQuery({
    queryKey: qk.keysPlayground,
    queryFn: () => apiFetch<{ data: KeyRow[] }>('/api/me/keys?limit=100&offset=0'),
    staleTime: 30_000,
  })
  return { ...query, choices: (query.data?.data ?? []).map((row) => toChoice(row)) }
}

/// 某分组 + 白名单下能调的模型：分组可见性与模型广场同口径，白名单与网关一致（精确匹配）。
export function modelsForKey(models: PricingModel[], group: string, allowlist: string[] | null): PricingModel[] {
  return models.filter((m) => isAvailable(m, group) && (allowlist === null || allowlist.includes(m.model)))
}
