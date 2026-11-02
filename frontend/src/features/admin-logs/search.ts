import { getRouteApi } from '@tanstack/react-router'
import type { LogSearch } from '@/routes/admin.logs'
import { validEntityId } from '@/features/entity-search/EntitySearchInput'

/// 检索条件（受控草稿 → 点查询才提交）。
///
/// 不做输入即查：全站明细查询打的是 CH raw 表，模型名敲到一半的每个前缀都
/// 发一发查询既慢又没意义；草稿/已提交两份状态，回车或点按钮才生效。
/// **已提交态 = URL search**（`routes/admin.logs.tsx`），草稿是本地表单值。
export interface Draft {
  model: string
  user_id: string
  api_key_id: string
  channel_id: string
  error_code: string
  request_id: string
  upstream_request_id: string
  group: string
  client_type: string
  log_type: string
  errors_only: boolean
  hours: number
  /// `datetime-local` 输入值（浏览器本地时区，形如 2026-08-30T00:00）；空串 = 用相对窗口
  from: string
  to: string
}

const DEFAULT_HOURS = 24

export const numericFilters = ['user_id', 'api_key_id', 'channel_id'] as const

export function invalidId(value: string): boolean {
  return !validEntityId(value)
}

export function validRange(draft: Pick<Draft, 'from' | 'to'>): boolean {
  const from = toIso(draft.from), to = toIso(draft.to)
  return from !== undefined && (draft.to === '' || (to !== undefined && Date.parse(to) >= Date.parse(from)))
}

/// RFC3339（UTC）→ datetime-local 输入值（本地时区，毫秒精度）。
function toLocalInput(iso: string | undefined): string {
  if (!iso) return ''
  const d = new Date(iso)
  if (Number.isNaN(d.getTime())) return ''
  const pad = (n: number) => String(n).padStart(2, '0')
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}.${String(d.getMilliseconds()).padStart(3, '0')}`
}

/// datetime-local → RFC3339（UTC）。地址栏与后端只认 UTC，避免时区随浏览器漂移。
function toIso(local: string): string | undefined {
  if (!local) return undefined
  const d = new Date(local)
  return Number.isNaN(d.getTime()) ? undefined : d.toISOString()
}

/// URL → 表单值（缺省字段回落为空串/缺省窗口）。
export function fromSearch(s: LogSearch): Draft {
  return {
    model: s.model ?? '',
    user_id: s.user_id?.toString() ?? '',
    api_key_id: s.api_key_id?.toString() ?? '',
    channel_id: s.channel_id?.toString() ?? '',
    error_code: s.error_code ?? '',
    request_id: s.request_id ?? '',
    upstream_request_id: s.upstream_request_id ?? '',
    group: s.group ?? '',
    client_type: s.client_type ?? '',
    log_type: s.log_type?.toString() ?? '',
    errors_only: s.errors_only === true,
    hours: s.hours ?? DEFAULT_HOURS,
    from: toLocalInput(s.from),
    to: toLocalInput(s.to),
  }
}

/// 表单值 → URL（空值不写进地址栏，保持链接干净；hours 等于缺省也省略）。
export function toSearch(d: Draft): LogSearch {
  const id = (v: string) => {
    const n = Number(v.trim())
    return Number.isInteger(n) && n > 0 ? n : undefined
  }
  const from = toIso(d.from)
  return {
    model: d.model.trim() || undefined,
    user_id: id(d.user_id),
    api_key_id: id(d.api_key_id),
    channel_id: id(d.channel_id),
    error_code: d.error_code.trim() || undefined,
    request_id: d.request_id.trim() || undefined,
    upstream_request_id: d.upstream_request_id.trim() || undefined,
    group: d.group.trim() || undefined,
    client_type: d.client_type.trim() || undefined,
    log_type: d.log_type ? Number(d.log_type) : undefined,
    errors_only: d.errors_only || undefined,
    // 绝对区间生效时相对窗口无意义，不写进地址
    hours: from !== undefined || d.hours === DEFAULT_HOURS ? undefined : d.hours,
    from,
    to: from === undefined ? undefined : toIso(d.to),
  }
}

export const routeApi = getRouteApi('/admin/logs')

export function toParams(f: Draft, offset: number, limit: number): string {
  const p = new URLSearchParams()
  const from = toIso(f.from)
  if (from !== undefined) {
    p.set('from', from)
    const to = toIso(f.to)
    if (to !== undefined) p.set('to', to)
  } else {
    p.set('hours', String(f.hours))
  }
  p.set('limit', String(limit))
  if (offset > 0) p.set('offset', String(offset))
  if (f.model.trim()) p.set('model', f.model.trim())
  if (f.user_id.trim()) p.set('user_id', f.user_id.trim())
  if (f.api_key_id.trim()) p.set('api_key_id', f.api_key_id.trim())
  if (f.channel_id.trim()) p.set('channel_id', f.channel_id.trim())
  if (f.error_code.trim()) p.set('error_code', f.error_code.trim())
  if (f.request_id.trim()) p.set('request_id', f.request_id.trim())
  for (const key of ['group', 'client_type', 'upstream_request_id', 'log_type'] as const) {
    if (f[key].trim()) p.set(key, f[key].trim())
  }
  if (f.errors_only) p.set('errors_only', 'true')
  return p.toString()
}
