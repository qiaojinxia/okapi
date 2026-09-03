export const RULE_TYPES = ['volume', 'time_based', 'discount', 'surge'] as const


export type RuleType = (typeof RULE_TYPES)[number]



/// 类型 → 文案键的显式映射（动态拼键会绕过文案闸门）。
export const TYPE_LABEL = {
  volume: 'admin:ruleTypeVolume',
  time_based: 'admin:ruleTypeTime',
  discount: 'admin:ruleTypeDiscount',
  surge: 'admin:ruleTypeSurge',
} as const


export const TYPE_HINT = {
  volume: 'admin:ruleTypeVolumeHint',
  time_based: 'admin:ruleTypeTimeHint',
  discount: 'admin:ruleTypeDiscountHint',
  surge: 'admin:ruleTypeSurgeHint',
} as const



/// 多命中叠加语义（对齐后端 Stacking::parse 白名单）。
export const STACKING_MODES = ['stackable', 'exclusive', 'best_for_user'] as const


export type StackingMode = (typeof STACKING_MODES)[number]


export const STACKING_LABEL = {
  stackable: 'admin:stackingStackable',
  exclusive: 'admin:stackingExclusive',
  best_for_user: 'admin:stackingBestForUser',
} as const



/// 星期 0=周日…6=周六 → 单字文案键（time_based weekdays 勾选与列表展示共用）。
export const WEEKDAY_LABEL = [
  'common:wkSun',
  'common:wkMon',
  'common:wkTue',
  'common:wkWed',
  'common:wkThu',
  'common:wkFri',
  'common:wkSat',
] as const

/// 规则行（/admin/pricing/rules 列表形状），抽屉编辑态据此回填。
export interface RuleRow {
  rule_code: string
  rule_type: string
  scope: { groups?: string[]; models?: string[]; users?: number[] }
  params: Record<string, unknown>
  priority: number
  enabled: boolean
  valid_from: string | null
  valid_to: string | null
}
