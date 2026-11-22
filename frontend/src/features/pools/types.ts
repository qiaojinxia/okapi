export const MEMBER_ROLES = ['primary', 'backup', 'canary'] as const
export type MemberRole = (typeof MEMBER_ROLES)[number]

export const AFFINITY_MODES = ['prefer_warm', 'strict', 'off'] as const
export type AffinityMode = (typeof AFFINITY_MODES)[number]

export interface SelectionPolicy {
  mode?: string | null
  respect_priority_layers?: boolean | null
  ignore_cost_in_weight?: boolean | null
}

export interface AffinityPolicy {
  mode?: AffinityMode | null
  ttl_secs?: number | null
}

export interface FailoverPolicy {
  max_attempts?: number | null
  max_same_channel_retries?: number | null
  first_output_timeout_secs?: number | null
}

export interface GuardrailPolicy {
  max_cost_milli?: number | null
  allowed_providers?: string[]
  denied_providers?: string[]
  required_tags?: string[]
  excluded_tags?: string[]
}

export interface RuleMatch {
  models?: string[]
  min_prompt_tokens?: number | null
  max_prompt_tokens?: number | null
  has_tools?: boolean | null
}

export interface RuleAction {
  selection?: SelectionPolicy | null
  affinity?: AffinityPolicy | null
  failover?: FailoverPolicy | null
  guardrails?: GuardrailPolicy | null
  canary_percent?: number | null
  override_pools?: string[] | null
}

export interface RoutingRule {
  name: string
  when: RuleMatch
  apply: RuleAction
}

/// 渠道池的声明式调度策略文档（与后端 `okapi_domain::pool_policy::PoolPolicy` 对齐）。
export interface PoolPolicy {
  fallback_pools?: string[]
  selection?: SelectionPolicy
  affinity?: AffinityPolicy
  failover?: FailoverPolicy
  guardrails?: GuardrailPolicy
  canary_percent?: number
  rules?: RoutingRule[]
}

export interface EffectivePoolPolicy {
  primary_pool: string
  pool_chain: string[]
  strategy: string
  respect_priority_layers: boolean
  ignore_cost_in_weight: boolean
  affinity_mode: string
  affinity_ttl_secs: number | null
  max_attempts: number | null
  max_same_channel_retries: number | null
  first_output_timeout_secs: number | null
  guardrails: GuardrailPolicy
  canary_percent: number
  matched_rules: string[]
}

export interface SelectedCandidateSummary {
  channel_id: number
  channel_key_id: number
  channel_name: string
  provider: string
  pool_rank: number
  pool_role: string
  priority: number
  weight: number
  cost_milli: number
  tags: string[]
}

export interface RejectedCandidate {
  channel_id: number
  channel_key_id: number
  channel_name: string
  provider: string
  pool_rank: number
  pool_role: string
  reason: string
}

export interface DecisionTrace {
  policy_version: number
  effective_policy: EffectivePoolPolicy
  canary_selected: boolean
  ordered_candidates: SelectedCandidateSummary[]
  rejected_candidates: RejectedCandidate[]
}

export interface SimulatePoolResponse {
  pool_code: string
  requested_model: string
  canonical_model: string
  saved: DecisionTrace
  draft: DecisionTrace | null
}

export interface PoolRow {
  pool_code: string
  description: string | null
  routing_strategy: string
  /// 本池无候选时退到的池（单跳）；null = 不降级。
  fallback_pool_code: string | null
  policy?: PoolPolicy
  policy_version?: number
  /// 内置 default 池：新渠道缺省加入、未指定池的分组走这里，不可删。
  builtin: boolean
  channel_count: number
  /// 引用该池的分组数 / 令牌数 / 把它当降级目标的池数；任一 >0 时删除会被后端拒绝。
  group_count: number
  key_count: number
  fallback_ref_count: number
}

/// GET /admin/pools/{code}：成员、能服务的模型并集、引用它的分组、生效策略文档。
export interface PoolDetail {
  pool_code: string
  description: string | null
  routing_strategy: string
  fallback_pool_code: string | null
  policy?: PoolPolicy
  policy_version?: number
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
  role?: MemberRole
  enabled?: boolean
  tags?: string[]
  model_filter?: string[] | null
  models: string[]
  active_keys: number
}

/// 渠道在某个调度池里的成员关系（写入形态）；覆盖为 null = 继承渠道 / key 自身。
export interface PoolMember {
  pool_code: string
  priority_override: number | null
  weight_override: number | null
  role?: MemberRole
  enabled?: boolean
  tags?: string[]
  model_filter?: string[] | null
}

export const DEFAULT_POOL = 'default'

/// 与库 CHECK 约束和后端 ROUTING_STRATEGIES 一致。
export const ROUTING_STRATEGIES = [
  'priority_weighted',
  'least_latency',
  'least_usage',
  'least_concurrency',
  'soonest_reset',
] as const

export type RoutingStrategy = (typeof ROUTING_STRATEGIES)[number]

/// 未知取值按默认策略显示，与后端 `Strategy::parse` 一致。
export function strategyOf(raw: string | null | undefined): RoutingStrategy {
  return ROUTING_STRATEGIES.find((s) => s === raw) ?? 'priority_weighted'
}

export function roleOf(raw: string | null | undefined): MemberRole {
  return MEMBER_ROLES.find((r) => r === raw) ?? 'primary'
}

export const ROLE_LABEL = {
  primary: 'admin:poolRolePrimary',
  backup: 'admin:poolRoleBackup',
  canary: 'admin:poolRoleCanary',
} as const

export const STRATEGY_LABEL = {
  priority_weighted: 'admin:strategyPriorityWeighted',
  least_latency: 'admin:strategyLeastLatency',
  least_usage: 'admin:strategyLeastUsage',
  least_concurrency: 'admin:strategyLeastConcurrency',
  soonest_reset: 'admin:strategySoonestReset',
} as const

export const STRATEGY_HINT = {
  priority_weighted: 'admin:strategyPriorityWeightedHint',
  least_latency: 'admin:strategyLeastLatencyHint',
  least_usage: 'admin:strategyLeastUsageHint',
  least_concurrency: 'admin:strategyLeastConcurrencyHint',
  soonest_reset: 'admin:strategySoonestResetHint',
} as const

export const AFFINITY_LABEL = {
  prefer_warm: 'admin:poolAffinityPreferWarm',
  strict: 'admin:poolAffinityStrict',
  off: 'admin:poolAffinityOff',
} as const

export function emptyPolicy(): PoolPolicy {
  return {
    fallback_pools: [],
    selection: {},
    affinity: {},
    failover: {},
    guardrails: {},
    canary_percent: 0,
    rules: [],
  }
}

/// 规整策略对象，去掉空值与无意义字段以严格匹配后端 `#[serde(deny_unknown_fields)]` 与校验规则。
export function normalizePolicyForSubmit(raw: PoolPolicy): PoolPolicy {
  const cleanList = (items?: string[] | null) =>
    (items ?? []).map((s) => s.trim()).filter(Boolean)

  const selection: SelectionPolicy = {}
  if (raw.selection?.mode) selection.mode = raw.selection.mode
  if (raw.selection?.respect_priority_layers === false) selection.respect_priority_layers = false
  if (raw.selection?.ignore_cost_in_weight === true) selection.ignore_cost_in_weight = true

  const affinity: AffinityPolicy = {}
  if (raw.affinity?.mode && raw.affinity.mode !== 'prefer_warm') affinity.mode = raw.affinity.mode
  if (raw.affinity?.ttl_secs !== null && raw.affinity?.ttl_secs !== undefined) {
    affinity.ttl_secs = raw.affinity.ttl_secs
  }

  const failover: FailoverPolicy = {}
  if (raw.failover?.max_attempts !== null && raw.failover?.max_attempts !== undefined) {
    failover.max_attempts = raw.failover.max_attempts
  }
  if (
    raw.failover?.max_same_channel_retries !== null &&
    raw.failover?.max_same_channel_retries !== undefined
  ) {
    failover.max_same_channel_retries = raw.failover.max_same_channel_retries
  }
  if (
    raw.failover?.first_output_timeout_secs !== null &&
    raw.failover?.first_output_timeout_secs !== undefined
  ) {
    failover.first_output_timeout_secs = raw.failover.first_output_timeout_secs
  }

  const cleanGuardrails = (g?: GuardrailPolicy | null): GuardrailPolicy => {
    const out: GuardrailPolicy = {}
    if (g?.max_cost_milli !== null && g?.max_cost_milli !== undefined) {
      out.max_cost_milli = g.max_cost_milli
    }
    const ap = cleanList(g?.allowed_providers)
    if (ap.length > 0) out.allowed_providers = ap
    const dp = cleanList(g?.denied_providers)
    if (dp.length > 0) out.denied_providers = dp
    const rt = cleanList(g?.required_tags)
    if (rt.length > 0) out.required_tags = rt
    const et = cleanList(g?.excluded_tags)
    if (et.length > 0) out.excluded_tags = et
    return out
  }

  const rules: RoutingRule[] = (raw.rules ?? [])
    .filter((r) => r.name.trim() !== '')
    .map((r) => {
      const when: RuleMatch = {}
      const models = cleanList(r.when.models)
      if (models.length > 0) when.models = models
      if (r.when.min_prompt_tokens !== null && r.when.min_prompt_tokens !== undefined) {
        when.min_prompt_tokens = r.when.min_prompt_tokens
      }
      if (r.when.max_prompt_tokens !== null && r.when.max_prompt_tokens !== undefined) {
        when.max_prompt_tokens = r.when.max_prompt_tokens
      }
      if (r.when.has_tools !== null && r.when.has_tools !== undefined) {
        when.has_tools = r.when.has_tools
      }

      const apply: RuleAction = {}
      if (r.apply.selection?.mode) apply.selection = { mode: r.apply.selection.mode }
      if (r.apply.affinity?.mode) apply.affinity = { mode: r.apply.affinity.mode }
      if (r.apply.failover?.max_attempts !== null && r.apply.failover?.max_attempts !== undefined) {
        apply.failover = { max_attempts: r.apply.failover.max_attempts }
      }
      const gr = cleanGuardrails(r.apply.guardrails)
      if (Object.keys(gr).length > 0) apply.guardrails = gr
      if (r.apply.canary_percent !== null && r.apply.canary_percent !== undefined) {
        apply.canary_percent = r.apply.canary_percent
      }
      const op = cleanList(r.apply.override_pools)
      if (op.length > 0) apply.override_pools = op
      return { name: r.name.trim(), when, apply }
    })

  return {
    fallback_pools: cleanList(raw.fallback_pools),
    selection,
    affinity,
    failover,
    guardrails: cleanGuardrails(raw.guardrails),
    canary_percent: Math.max(0, Math.min(100, Math.round(raw.canary_percent ?? 0))),
    rules,
  }
}
