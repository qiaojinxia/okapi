import type { AccountControl } from '../types'

export const defaults: AccountControl = {
  quota_aware: false, quota_threshold_pct: 90,
  rate_limit_cooldown_secs: 60, failure_threshold: 3, failure_cooldown_secs: 60,
  refresh_mode: 'managed', refresh_margin_secs: 120,
}
export const commonNumbers = [
  ['rate_limit_cooldown_secs', 'channelRateCooldown', 1, 604800],
  ['failure_threshold', 'channelFailureThreshold', 1, 20],
  ['failure_cooldown_secs', 'channelFailureCooldown', 1, 7200],
] as const

/** Empty means unlimited; never round fractional or unsafe integer limits. */
export function parseLimit(text: string): number | undefined | null {
  const trimmed = text.trim()
  if (trimmed === '') return undefined
  if (!/^\d+$/.test(trimmed)) return null
  const value = Number(trimmed)
  return Number.isSafeInteger(value) && value > 0 ? value : null
}
