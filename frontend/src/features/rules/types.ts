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
