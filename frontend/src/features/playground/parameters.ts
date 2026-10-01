import { ApiError, getKey } from '@/lib/api'

export interface ParameterProfile {
  known: boolean
  temperature_max: number | null
  top_p: boolean
  sampling_requires_none: boolean
  sampling_requires_no_budget: boolean
  efforts: string[]
  default_effort: string | null
  budget_min: number | null
  budget_max: number | null
  preserve_reasoning: boolean
}

// Missing/old backend: don't guess model support or send remembered controls.
export const DEFAULT_PARAMETERS: ParameterProfile = {
  known: false, temperature_max: null, top_p: false, sampling_requires_none: false, sampling_requires_no_budget: false,
  efforts: [], default_effort: null, budget_min: null, budget_max: null, preserve_reasoning: false,
}

export async function loadParameters(model: string, keyId: number | null, signal: AbortSignal): Promise<ParameterProfile> {
  const key = getKey()
  const response = await fetch(`/api/me/playground/parameters?model=${encodeURIComponent(model)}`, {
    signal,
    headers: { ...(key ? { Authorization: `Bearer ${key}` } : {}), ...(keyId === null ? {} : { 'X-Okapi-Playground-Key': String(keyId) }) },
  })
  const value = await response.json()
  if (!response.ok) throw new ApiError(response.status, value.error?.code ?? 'internal_error', value.error?.param)
  if (typeof value.known !== 'boolean' || !Array.isArray(value.efforts)
    || !value.efforts.every((e: unknown) => typeof e === 'string')
    || !(value.temperature_max === null || (typeof value.temperature_max === 'number' && Number.isFinite(value.temperature_max)))
    || typeof value.top_p !== 'boolean' || typeof value.sampling_requires_none !== 'boolean'
    || typeof value.sampling_requires_no_budget !== 'boolean'
    || !(value.default_effort === null || typeof value.default_effort === 'string')
    || ![value.budget_min, value.budget_max].every((n) => n === null || (Number.isSafeInteger(n) && n > 0))
    || typeof value.preserve_reasoning !== 'boolean') throw new ApiError(502, 'internal_error')
  return value as ParameterProfile
}

export function samplingAllowed(profile: ParameterProfile, effort: string): boolean {
  return !profile.sampling_requires_none || (effort || profile.default_effort) === 'none'
}
