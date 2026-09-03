export interface PoolRow {
  pool_code: string
  description: string | null
  routing_strategy: string
  /// 本池无候选时退到的池（单跳）；null = 不降级。
  fallback_pool_code: string | null
  /// 内置 default 池：新渠道缺省加入、未指定池的分组走这里，不可删。
  builtin: boolean
  channel_count: number
  /// 引用该池的分组数 / 令牌数 / 把它当降级目标的池数；任一 >0 时删除会被后端拒绝。
  group_count: number
  key_count: number
  fallback_ref_count: number
}

/// GET /admin/pools/{code}：成员、能服务的模型并集、引用它的分组。
export interface PoolDetail {
  pool_code: string
  description: string | null
  routing_strategy: string
  fallback_pool_code: string | null
  members: PoolMemberRow[]
  models: string[]
  groups: string[]
}

export interface PoolMemberRow {
  channel_id: number
  name: string
  provider: string
  status: number
  priority: number
  priority_override: number | null
  weight_override: number | null
  models: string[]
  active_keys: number
}

/// 渠道在某个池里的成员关系（写入形态）；覆盖为 null = 继承渠道 / key 自身。
export interface PoolMember {
  pool_code: string
  priority_override: number | null
  weight_override: number | null
}

export const DEFAULT_POOL = 'default'

/// 与库 CHECK 约束和后端 ROUTING_STRATEGIES 一致。
export const ROUTING_STRATEGIES = ['priority_weighted', 'least_latency'] as const

export const STRATEGY_LABEL = {
  priority_weighted: 'admin:strategyPriorityWeighted',
  least_latency: 'admin:strategyLeastLatency',
} as const

export const STRATEGY_HINT = {
  priority_weighted: 'admin:strategyPriorityWeightedHint',
  least_latency: 'admin:strategyLeastLatencyHint',
} as const
