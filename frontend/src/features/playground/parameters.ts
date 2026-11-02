import { ApiError, apiFetch } from '@/lib/api'

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
  // 走 apiFetch：登录 key 失效时与其它接口一样触发复核与登出，不再自己拼鉴权头
  const value = await apiFetch<Record<string, unknown> | undefined>(`/api/me/playground/parameters?model=${encodeURIComponent(model)}`, {
    signal,
    headers: keyId === null ? {} : { 'X-Okapi-Playground-Key': String(keyId) },
  }).catch((err: unknown) => {
    // 成功状态码配非 JSON 体（网关前面的代理回的 HTML 页）：当作上游异常，而不是抛 SyntaxError
    throw err instanceof SyntaxError ? new ApiError(502, 'internal_error') : err
  })
  if (value === undefined || value === null || typeof value !== 'object') throw new ApiError(502, 'internal_error')
  const budget = (n: unknown) => n === null || (typeof n === 'number' && Number.isSafeInteger(n) && n > 0)
  if (typeof value.known !== 'boolean' || !Array.isArray(value.efforts)
    || !value.efforts.every((e: unknown) => typeof e === 'string')
    || !(value.temperature_max === null || (typeof value.temperature_max === 'number' && Number.isFinite(value.temperature_max)))
    || typeof value.top_p !== 'boolean' || typeof value.sampling_requires_none !== 'boolean'
    || typeof value.sampling_requires_no_budget !== 'boolean'
    || !(value.default_effort === null || typeof value.default_effort === 'string')
    || !budget(value.budget_min) || !budget(value.budget_max)
    || typeof value.preserve_reasoning !== 'boolean') throw new ApiError(502, 'internal_error')
  return value as unknown as ParameterProfile
}

export function samplingAllowed(profile: ParameterProfile, effort: string): boolean {
  return !profile.sampling_requires_none || (effort || profile.default_effort) === 'none'
}
