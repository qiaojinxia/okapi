import { isRecord, scaledInteger } from './setting-catalog'

export interface SettingField {
  key: string
  label: string
  kind?: 'secret' | 'url' | 'rate'
  optional?: boolean
  placeholder?: string
}

export const EPAY_FIELDS: SettingField[] = [
  { key: 'gateway_url', label: 'admin:settingGatewayUrl', kind: 'url', placeholder: 'https://pay.example.com/submit.php' },
  { key: 'pid', label: 'admin:settingMerchantId' },
  { key: 'key', label: 'admin:settingMerchantKey', kind: 'secret' },
  { key: 'usd_to_cny_milli', label: 'admin:settingExchangeRate', kind: 'rate', optional: true, placeholder: '7' },
]
export const STRIPE_FIELDS: SettingField[] = [
  { key: 'secret_key', label: 'admin:settingStripeKey', kind: 'secret' },
  { key: 'webhook_secret', label: 'admin:settingWebhookSecret', kind: 'secret' },
  { key: 'api_base', label: 'admin:settingApiBase', kind: 'url', optional: true, placeholder: 'https://api.stripe.com' },
]
export const OAUTH_FIELDS: SettingField[] = [
  { key: 'code', label: 'admin:settingProviderCode', placeholder: 'github / discord / linuxdo' },
  { key: 'client_id', label: 'admin:settingClientId' },
  { key: 'client_secret', label: 'admin:settingClientSecret', kind: 'secret' },
]
export const OAUTH_OPTIONAL_FIELDS: SettingField[] = [
  { key: 'authorize_url', label: 'admin:settingAuthorizeUrl', kind: 'url', optional: true },
  { key: 'token_url', label: 'admin:settingTokenUrl', kind: 'url', optional: true },
  { key: 'userinfo_url', label: 'admin:settingUserinfoUrl', kind: 'url', optional: true },
  { key: 'scopes', label: 'admin:settingScopes', optional: true },
  { key: 'subject_field', label: 'admin:settingSubjectField', optional: true },
  { key: 'display_field', label: 'admin:settingDisplayField', optional: true },
]

export function fieldText(value: unknown, field: SettingField): string {
  if (field.kind === 'rate' && typeof value === 'number') return String(value / 1000)
  return value === undefined || value === null ? '' : String(value)
}

export function initialFields(value: unknown, fields: SettingField[]): Record<string, unknown> {
  const obj = isRecord(value) ? value : {}
  return { ...obj, ...Object.fromEntries(fields.map((field) => [field.key, fieldText(obj[field.key], field)])) }
}

export type FormError = { key: string; field?: string }
export function validUrl(raw: string): boolean {
  try {
    const url = new URL(raw)
    return ['https:', 'http:'].includes(url.protocol) && !!url.hostname && !url.username && !url.password
  } catch { return false }
}

export function parseFields(draft: Record<string, unknown>, fields: SettingField[]): { value: Record<string, unknown>; error?: FormError } {
  const value = { ...draft }
  for (const field of fields) {
    const raw = String(draft[field.key] ?? '')
    if (raw.trim() === '') {
      if (!field.optional) return { value, error: { key: 'admin:settingRequired', field: field.label } }
      delete value[field.key]
    } else if (field.kind === 'rate') {
      const n = scaledInteger(raw, 3)
      if (n === null || n <= 0) return { value, error: { key: 'admin:settingInvalidRate' } }
      value[field.key] = n
    } else {
      if (field.kind === 'url' && !validUrl(raw.trim())) return { value, error: { key: 'admin:settingInvalidUrl', field: field.label } }
      value[field.key] = field.kind === 'secret' ? raw : raw.trim()
    }
  }
  return { value }
}
