import { useEffect, useState } from 'react'
import type { AccountControl } from '../types'
import type { AccountCapabilities, TokenPeriod } from './api'
import { commonNumbers, defaults, parseLimit } from './policy'

export function useControlDraft(value: AccountControl | undefined, capabilities: AccountCapabilities | undefined,
  refreshable: boolean, onChange: (value: AccountControl) => void, onValidChange: (valid: boolean) => void) {
  const { usage: legacyUsage, ...saved } = value ?? {}
  const policy = { ...defaults, ...saved }
  const subscription = capabilities?.subscription
  const [quotaDrafts, setQuotaDrafts] = useState<Record<string, string>>(() => Object.fromEntries(Object.entries(value?.quota_limits ?? {}).map(([seconds, cap]) => [seconds, String(cap)])))
  const [tokenDraft, setTokenDraft] = useState(() => value?.local_tokens ? String(value.local_tokens.cap) : '')
  const [tokenPeriod, setTokenPeriod] = useState<TokenPeriod>(() => value?.local_tokens?.period ?? 'total')
  const quotaWindows = subscription?.quota_windows ?? (subscription?.window_secs ? [subscription.window_secs] : [])
  const legacyWindow = subscription?.window_secs ?? quotaWindows.at(-1)
  const quotaText = (seconds: number) => quotaDrafts[String(seconds)] ?? (policy.quota_aware && seconds === legacyWindow ? String(policy.quota_threshold_pct) : '')
  const [numbers, setNumbers] = useState(() => ({
    rate_limit_cooldown_secs: String(policy.rate_limit_cooldown_secs),
    failure_threshold: String(policy.failure_threshold), failure_cooldown_secs: String(policy.failure_cooldown_secs),
    refresh_margin_secs: String(policy.refresh_margin_secs),
  }))
  const validNumber = (name: keyof typeof numbers, min: number, max: number) =>
    /^\d+$/.test(numbers[name]) && Number.isSafeInteger(Number(numbers[name])) && Number(numbers[name]) >= min && Number(numbers[name]) <= max
  const commonValid = commonNumbers.every(([name, , min, max]) => validNumber(name, min, max))
  const quotaValid = !subscription || !capabilities?.quota || quotaWindows.every((seconds) => {
    const limit = parseLimit(quotaText(seconds))
    return limit !== null && (limit === undefined || limit <= 100)
  })
  const tokenLimit = parseLimit(tokenDraft)
  const tokenValid = !subscription || tokenLimit !== null
  const refreshValid = !subscription || !capabilities?.refresh || !refreshable || policy.refresh_mode !== 'managed' || validNumber('refresh_margin_secs', 120, 3600)
  useEffect(() => { onValidChange(commonValid && quotaValid && tokenValid && refreshValid) }, [commonValid, quotaValid, tokenValid, refreshValid, onValidChange])
  const changeNumber = (name: keyof typeof numbers, text: string, min: number, max: number) => {
    setNumbers((current) => ({ ...current, [name]: text }))
    const number = Number(text)
    if (/^\d+$/.test(text) && Number.isSafeInteger(number) && number >= min && number <= max) onChange({ ...policy, [name]: number })
  }
  const changeQuota = (seconds: number, text: string) => {
    const next = { ...quotaDrafts, ...Object.fromEntries(quotaWindows.map((window) => [String(window), quotaText(window)])), [String(seconds)]: text }
    setQuotaDrafts(next)
    // Hidden drafts belong to other plugin windows. Preserve their saved limits, but
    // do not let an invalid hidden draft prevent a valid visible edit from being saved.
    const parsed = quotaWindows.map((window) => [String(window), parseLimit(next[String(window)] ?? '')] as const)
    if (parsed.every(([, cap]) => cap !== null && (cap === undefined || cap <= 100))) {
      const limits = { ...policy.quota_limits }
      for (const [window, cap] of parsed) {
        if (typeof cap === 'number') limits[window] = cap
        else delete limits[window]
      }
      onChange({ ...policy, quota_aware: false, quota_limits: limits })
    }
  }
  const changeTokens = (text: string) => {
    setTokenDraft(text)
    const cap = parseLimit(text)
    if (cap !== null) onChange({ ...policy, local_tokens: cap === undefined ? null : { cap, period: tokenPeriod } })
  }
  const changePeriod = (period: TokenPeriod) => {
    setTokenPeriod(period)
    if (tokenLimit !== null && tokenLimit !== undefined) onChange({ ...policy, local_tokens: { cap: tokenLimit, period } })
  }
  const setRenewal = (enabled: boolean) => onChange({ ...policy, refresh_mode: enabled ? 'managed' : 'external' })
  const commonChanged = commonNumbers.some(([name]) => policy[name] !== defaults[name])
  return { policy, legacyUsage, numbers, validNumber, commonValid, commonChanged,
    quotaWindows, quotaText, quotaValid, changeQuota, tokenDraft, tokenPeriod, tokenValid,
    changeTokens, changePeriod, refreshValid, changeNumber, setRenewal }
}
export type ControlDraft = ReturnType<typeof useControlDraft>
