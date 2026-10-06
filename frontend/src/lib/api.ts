// API client：Bearer key 鉴权；后端只回 error_code + param（i18n 红线），
// 文案渲染在 errors 命名空间完成。

const KEY_STORAGE = 'okapi.key'
const LOGIN_STORAGE = 'okapi.login-mode'
export const USAGE_SCOPE_STORAGE = 'okapi.usage-scope'

const authResets = new Set<() => void>()
let authEpoch = 0
export function registerAuthReset(reset: () => void): () => void {
  authResets.add(reset)
  return () => authResets.delete(reset)
}
function resetAuthContext() {
  authEpoch += 1
  for (const reset of authResets) reset()
}
if (typeof window !== 'undefined') window.addEventListener('storage', (event) => {
  if (event.key === KEY_STORAGE) resetAuthContext()
})

export function getLoginMode(): 'account' | 'key' | null {
  const value = localStorage.getItem(LOGIN_STORAGE)
  return value === 'account' || value === 'key' ? value : null
}

export function getKey(): string | null {
  return localStorage.getItem(KEY_STORAGE)
}

export function setKey(key: string, mode: 'account' | 'key' = 'key'): void {
  resetAuthContext()
  localStorage.setItem(KEY_STORAGE, key)
  localStorage.setItem(LOGIN_STORAGE, mode)
  localStorage.removeItem(USAGE_SCOPE_STORAGE)
}

export function clearKey(): void {
  resetAuthContext()
  localStorage.removeItem(KEY_STORAGE)
  localStorage.removeItem(LOGIN_STORAGE)
  localStorage.removeItem(USAGE_SCOPE_STORAGE)
}

export class ApiError extends Error {
  readonly code: string
  readonly param: string | undefined
  readonly status: number

  constructor(status: number, code: string, param?: string) {
    super(code)
    this.status = status
    this.code = code
    this.param = param
  }
}

interface ErrorEnvelope {
  error?: { code?: string; param?: string; type?: string; message?: string }
}

export async function apiFetch<T>(
  path: string,
  init?: { method?: string; body?: unknown; key?: string; fresh?: boolean; signal?: AbortSignal },
): Promise<T> {
  const epoch = authEpoch
  const key = init?.key ?? getKey()
  const headers: Record<string, string> = {}
  if (key) headers.Authorization = `Bearer ${key}`
  if (init?.body !== undefined) headers['Content-Type'] = 'application/json'
  if (init?.fresh) headers['Cache-Control'] = 'no-cache'

  const resp = await fetch(path, {
    method: init?.method ?? 'GET',
    headers,
    body: init?.body === undefined ? undefined : JSON.stringify(init.body),
    signal: init?.signal,
  })
  if (!resp.ok) {
    let code = `http_${resp.status}`
    let param: string | undefined
    try {
      const body = (await resp.json()) as ErrorEnvelope
      code = body.error?.code ?? body.error?.type ?? code
      param = body.error?.param
    } catch {
      // 非 JSON 错误体：保留 http_<status>
    }
    throw new ApiError(resp.status, code, param)
  }
  const result = (await resp.json()) as T
  if (epoch !== authEpoch) throw new ApiError(401, 'auth_context_changed')
  return result
}
