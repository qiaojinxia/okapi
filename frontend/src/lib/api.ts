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

const authExpiredHandlers = new Set<() => void>()
/// 存着的登录 key 失效时回调（401 invalid_api_key / key_disabled：在别的设备上被删除、被停用或过期）。
/// 外壳据此清掉本地 key 回登录页，而不是让每个页面各报一遍「API Key 无效」。
export function registerAuthExpired(handler: () => void): () => void {
  authExpiredHandlers.add(handler)
  return () => authExpiredHandlers.delete(handler)
}
const KEY_REVOKED_CODES = new Set(['invalid_api_key', 'key_disabled'])
let verifyingKey: Promise<unknown> | null = null

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
    // 只认用存着的登录 key 发出、且它此刻仍是当前 key 的请求：显式传 key 的探测（粘贴 key 登录）不算；
    // 回调清掉 key 之后 getKey() 就变了，同时失败的其余请求不再重复触发。
    // 会话类接口（复制完整 key、两步验证、资料）缺网页会话时也回 invalid_api_key，那时 key 本身是好的：
    // 只有 /api/me（纯按 key 鉴权）失败才算 key 失效，其余接口先用它复核一次。
    if (resp.status === 401 && KEY_REVOKED_CODES.has(code) && init?.key === undefined && key !== null && key === getKey()) {
      if (path.split('?')[0] === '/api/me') {
        for (const handler of authExpiredHandlers) handler()
      } else if (verifyingKey === null && authExpiredHandlers.size > 0) {
        verifyingKey = apiFetch('/api/me').catch(() => undefined).finally(() => { verifyingKey = null })
      }
    }
    throw new ApiError(resp.status, code, param)
  }
  const result = (await resp.json()) as T
  if (epoch !== authEpoch) throw new ApiError(401, 'auth_context_changed')
  return result
}
