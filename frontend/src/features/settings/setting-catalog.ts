export interface SettingRow {
  key: string
  value: unknown
  is_secret: boolean
  configured: boolean
  updated_at: string | null
}

export type SettingsSection = 'registration' | 'notice' | 'notify'
export type SettingEditor = 'auto' | 'percent' | 'epay' | 'stripe' | 'limits' | 'ssrf' | 'oauth'
export type SettingGroup = 'payment' | 'identity' | 'traffic' | 'security' | 'other'

export const SETTING_GROUPS = [
  { id: 'payment', label: 'admin:settingGroupPayment' },
  { id: 'identity', label: 'admin:settingGroupIdentity' },
  { id: 'traffic', label: 'admin:settingGroupTraffic' },
  { id: 'security', label: 'admin:settingGroupSecurity' },
  { id: 'other', label: 'admin:settingGroupOther' },
] as const

export interface SettingMeta {
  label: string
  description: string
  group: SettingGroup
  editor: SettingEditor
  section?: SettingsSection
}

const CATALOG: Record<string, SettingMeta> = {
  aff_percent_bp: { label: 'admin:settingReferral', description: 'admin:settingReferralDesc', group: 'payment', editor: 'percent' },
  payment_epay: { label: 'admin:settingEpay', description: 'admin:settingEpayDesc', group: 'payment', editor: 'epay' },
  payment_stripe: { label: 'admin:settingStripe', description: 'admin:settingStripeDesc', group: 'payment', editor: 'stripe' },
  oauth_providers: { label: 'admin:settingOAuth', description: 'admin:settingOAuthDesc', group: 'identity', editor: 'oauth' },
  notify_channels: { label: 'admin:notify', description: 'admin:settingNotifyDesc', group: 'identity', editor: 'auto', section: 'notify' },
  registration_policy: { label: 'admin:regTitle', description: 'admin:settingRegistrationDesc', group: 'identity', editor: 'auto', section: 'registration' },
  site_notice: { label: 'admin:noticeTitle', description: 'admin:settingNoticeDesc', group: 'identity', editor: 'auto', section: 'notice' },
  model_rpm_limits: { label: 'admin:settingModelLimits', description: 'admin:settingModelLimitsDesc', group: 'traffic', editor: 'limits' },
  mcp_write_enabled: { label: 'admin:settingMcpWrite', description: 'admin:settingMcpWriteDesc', group: 'security', editor: 'auto' },
  ssrf_policy: { label: 'admin:settingAccess', description: 'admin:settingAccessDesc', group: 'security', editor: 'ssrf' },
}

export function settingMeta(key: string): SettingMeta {
  if (Object.hasOwn(CATALOG, key)) return CATALOG[key]
  if (key.startsWith('epay_key_')) return {
    label: 'admin:settingPaymentKey', description: 'admin:settingPaymentKeyDesc', group: 'payment', editor: 'auto',
  }
  return { label: '', description: 'admin:settingOtherDesc', group: 'other', editor: 'auto' }
}

export function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

// 未知对象/数组不序列化到列表或 title 属性。
export function containsSecret(value: unknown): boolean {
  if (Array.isArray(value)) return value.some(containsSecret)
  if (!isRecord(value)) return false
  return Object.entries(value).some(([key, item]) =>
    /secret|password|credential|token|(^|_)key($|_)/i.test(key) || containsSecret(item),
  )
}

// 十进制文本精确转为整数单位，不把空输入和格式错误悄悄写成 0。
export function scaledInteger(text: string, places: number): number | null {
  const match = /^(\d+)(?:\.(\d+))?$/.exec(text.trim())
  if (!match || (match[2]?.length ?? 0) > places) return null
  const n = Number(match[1] + (match[2] ?? '').padEnd(places, '0'))
  return Number.isSafeInteger(n) ? n : null
}

export function decimalText(value: unknown, scale = 1): string {
  return typeof value === 'number' ? String(value / scale) : ''
}
