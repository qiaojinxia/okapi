export interface PoolRow {
  pool_code: string
  description: string | null
  routing_strategy: string
  channel_count: number
  /// 引用该池的分组数与令牌数；任一 >0 时删除会被后端拒绝。
  group_count: number
  key_count: number
}

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
