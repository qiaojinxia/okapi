import type { CompatLayer, CompatRule, EffectiveRule } from '@/features/channels/compat'

export type { CompatLayer, CompatRule, EffectiveRule }

/// 规则集的归属：渠道层（抽屉里已有入口）、适配档案、全局护栏（接口里 id 写 0）。
export type SetScope = 'channel' | 'profile' | 'guard'

export type Phase = 'request_pre' | 'request' | 'wire' | 'response' | 'error' | 'capability'

/// 规则集的一个版本（接口的 `row_json`）。
export interface SetRow {
  version: number
  status: 'draft' | 'published' | 'archived'
  rules: CompatRule[]
  note: string | null
  created_by: number | null
  created_at: string
  published_by: number | null
  published_at: string | null
  /// 草稿起草时所依据的已发布版本（没有 = 0）。
  base_version: number | null
  /// `admin`（人在页面上写的）或 `mcp`（AI 提议的）。
  source: string
  rationale: string | null
}

export interface SetOverview {
  scope: SetScope
  id: number
  published: SetRow | null
  draft: SetRow | null
  /// 草稿起草时的已发布版本与现在对不上。
  stale: boolean
}

/// 版本历史里的一行：规则内容不带，只带条数。
export type VersionRow = Omit<SetRow, 'rules'> & { rules: number }

export interface InboxItem {
  scope: SetScope
  id: number
  name: string | null
  version: number
  base_version: number | null
  published_version: number | null
  stale: boolean
  source: string
  rationale: string | null
  note: string | null
  created_by: number | null
  created_at: string
  rules: number
}

export interface ProfileRow {
  id: number
  slug: string
  name: string
  description: string | null
  /// `builtin:<id>`；没叠加厂商为 null。
  extends: string | null
  hosts: string[]
  /// 渠道 `vendor` 字段里的写法：`custom:<slug>`。
  vendor: string
  created_by: number | null
  created_at: string
  updated_at: string
  /// 线上版本号；还没发布过为 null。
  version: number | null
  rules: number
  has_draft: boolean
  channels: number
}

export interface EffectiveView {
  layers: { layer: CompatLayer; label: string; rules: number }[]
  rules: EffectiveRule[]
  shadowed: { id: string; layer: CompatLayer; by: CompatLayer; reason: 'overridden' | 'disabled' }[]
}

export type ProfileDetail = Omit<ProfileRow, 'version' | 'rules' | 'has_draft'> & {
  published: SetRow | null
  draft: SetRow | null
  effective: EffectiveView
}

export interface Endpoint {
  protocol: string
  base: string
  label?: string
}

/// 声明式探针（`probes.balance`）：只用来在「参考」页展示，字段以后端为准、读不到当没有。
export interface BalanceProbe {
  requests: { path: string; root?: boolean; query?: Record<string, unknown> }[]
  remaining?: { from?: number; at: string; divide?: number }
  total?: { from?: number; at: string; divide?: number }
  used?: { from?: number; at: string; divide?: number }
  currency: unknown
  doc?: { why?: string; url?: string; verified_at?: string }
}

export interface VendorInfo {
  id: string
  display_name: string
  group: 'cn' | 'western' | 'self_hosted'
  hosts: string[]
  endpoints: Endpoint[]
  probes: { balance?: BalanceProbe }
  rules: number
}

export interface Suggestion {
  id: string
  display_name: string
  kind: 'builtin' | 'profile'
}

export interface CatalogField {
  name: string
  type: string
  required: boolean
  note: string
}

export interface CatalogOp {
  name: string
  phases: Phase[]
  summary: string
  fields: CatalogField[]
}

export interface CatalogPhase {
  id: Phase
  enabled: boolean
  summary: string
  note: string
  ops: string[]
}

export interface ProtectedPath {
  path: string
  class: 'free' | 'sensitive' | 'billing_critical' | 'protected'
  node_only: boolean
}

export interface Recipe {
  id: string
  title: string
  rule: CompatRule
}

/// 规则的自描述目录（`GET /admin/compat/catalog`）；蓝图的节点库、配方、参考页都读它。
export interface Catalog {
  schema_version: number
  phases: CatalogPhase[]
  ops: CatalogOp[]
  selector: {
    fields: { name: string; type: string; note?: string; values?: string[] }[]
    capability_fields: string[]
    response_dialects: string[]
  }
  when: { fields: { name: string; type: string; note?: string }[]; semantics: string }
  usage_slots: { name: string; pointer: string }[]
  capability_facts: string[]
  protected_paths: { request: ProtectedPath[]; response: ProtectedPath[] }
  limits: Record<string, number>
  recipes: Recipe[]
}

/// 校验问题（`validate::Issue`）：`detail` 是引擎给的技术细节，界面主要靠 `code`。
export interface RuleIssue {
  code: string
  rule?: string
  index?: number
  detail: string
}

export interface Fired {
  rule: string
  layer: CompatLayer
  /// 真正改变的节点个数。
  changes: number
}

export interface Skipped {
  rule: string
  layer: CompatLayer
  reason: string
}

export interface Reaction {
  rule: string
  layer: CompatLayer
  action: 'retry_same' | 'failover' | 'fail'
  viable: boolean
  detail: Record<string, unknown>
}

/// 试跑的结果（`POST /admin/compat/dry-run`）：规则不合法时只有 `valid: false` 与问题清单。
export interface DryRunResult {
  valid: boolean
  issues?: RuleIssue[]
  phase?: Phase
  layers?: { layer: CompatLayer; label: string; rules: number }[]
  changed?: boolean
  after?: unknown
  fired?: Fired[]
  skipped?: Skipped[]
  matched?: boolean
  chosen?: number | null
  reactions?: Reaction[]
  facts?: Record<string, boolean>
  channel_capabilities?: unknown
  merged?: unknown
}

export interface DryRunContext {
  model: string
  dialect: string
  operation: string
  stream: boolean
  host?: string
  vendor?: string
  client_model?: string
  client?: string
}

export interface DryRunBody {
  channel_id?: number
  vendor?: string
  rules?: CompatRule[]
  phase?: Phase
  context: DryRunContext
  request?: unknown
  response?: unknown
  error?: { status: number; body?: unknown }
}
