import { isRecord } from './setting-catalog'
import type { FormError } from './setting-fields'

export const REFRESH_FIELDS = [
  { key: 'interval_secs', label: 'admin:oauthPolicyInterval', min: 5, max: 300, default: 30 },
  { key: 'refresh_margin_secs', label: 'admin:oauthPolicyMargin', min: 120, max: 3600, default: 300 },
  { key: 'batch_size', label: 'admin:oauthPolicyBatch', min: 1, max: 1000, default: 100 },
  { key: 'concurrency', label: 'admin:oauthPolicyConcurrency', min: 1, max: 8, default: 2 },
  { key: 'requests_per_second', label: 'admin:oauthPolicyRate', min: 1, max: 5, default: 1 },
] as const

export function initialRefreshPolicy(value: unknown): Record<string, unknown> {
  const raw = isRecord(value) ? value : {}
  return { ...raw, enabled: raw.enabled ?? true,
    ...Object.fromEntries(REFRESH_FIELDS.map((f) => [f.key, String(raw[f.key] ?? f.default)])),
  }
}

export function parseRefreshPolicy(draft: Record<string, unknown>): { value: Record<string, unknown>; error?: FormError } {
  const value: Record<string, unknown> = { ...draft, enabled: draft.enabled === true }
  for (const field of REFRESH_FIELDS) {
    const raw = String(draft[field.key] ?? '').trim(), n = Number(raw)
    if (!/^\d+$/.test(raw) || !Number.isSafeInteger(n) || n < field.min || n > field.max) {
      return { value, error: { key: 'admin:oauthPolicyRange', field: field.label } }
    }
    value[field.key] = n
  }
  return { value }
}
