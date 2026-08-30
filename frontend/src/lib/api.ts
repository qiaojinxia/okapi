// API client：Bearer key 鉴权；后端只回 error_code + param（i18n 红线），
// 文案渲染在 errors 命名空间完成。

const KEY_STORAGE = 'okapi.key'

export function getKey(): string | null {
  return localStorage.getItem(KEY_STORAGE)
}

export function setKey(key: string): void {
  localStorage.setItem(KEY_STORAGE, key)
}

export function clearKey(): void {
  localStorage.removeItem(KEY_STORAGE)
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
  init?: { method?: string; body?: unknown; key?: string },
): Promise<T> {
  const key = init?.key ?? getKey()
  const headers: Record<string, string> = {}
  if (key) headers.Authorization = `Bearer ${key}`
  if (init?.body !== undefined) headers['Content-Type'] = 'application/json'

  const resp = await fetch(path, {
    method: init?.method ?? 'GET',
    headers,
    body: init?.body === undefined ? undefined : JSON.stringify(init.body),
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
  return (await resp.json()) as T
}
