import { apiFetch } from '@/lib/api'

export interface ConversationArchivePolicy {
  enabled: boolean
  retention_days: number
  allow_user_view: boolean
  max_body_kb: number
  cleanup_interval_hours: number
}

export const DEFAULT_ARCHIVE_POLICY: ConversationArchivePolicy = {
  enabled: false,
  retention_days: 30,
  allow_user_view: false,
  max_body_kb: 256,
  cleanup_interval_hours: 6,
}

export interface ArchiveStatsResp {
  backend: 'clickhouse' | 'postgres'
  clickhouse_enabled: boolean
  total_count: number
  pg_count: number
  ch_count: number | null
  oldest_at: string | null
  newest_at: string | null
  policy: ConversationArchivePolicy
}

export interface ArchiveSummary {
  request_id: string
  user_id: number
  username?: string
  api_key_id: number
  channel_id: number
  channel_name?: string
  model: string
  requested_model: string
  endpoint: string
  is_stream: boolean
  status_code: number
  error_code: string
  request_preview: string
  response_preview: string
  truncated: boolean
  created_at: string
  storage: 'clickhouse' | 'postgres' | string
}

export interface ArchiveRecord {
  request_id: string
  user_id: number
  api_key_id: number
  channel_id: number
  model: string
  requested_model: string
  endpoint: string
  is_stream: boolean
  status_code: number
  error_code: string
  request_body: string
  response_body: string
  response_text: string
  reasoning_text: string
  tool_calls: string
  truncated: boolean
  created_at: string
  storage: 'clickhouse' | 'postgres' | string
}

export interface ArchiveCleanupResp {
  ok: boolean
  retention_days: number
  pg_deleted: number
  ch_deleted_requested: boolean
  cutoff: string | null
}

export interface ParsedToolCall {
  id: string
  name: string
  arguments: string
}

export interface ParsedChatTurn {
  role: 'system' | 'user' | 'assistant' | 'tool'
  text: string
  toolName?: string
}

export function fetchArchiveStats(): Promise<ArchiveStatsResp> {
  return apiFetch<ArchiveStatsResp>('/admin/conversations/stats')
}

export function runArchiveCleanup(): Promise<ArchiveCleanupResp> {
  return apiFetch<ArchiveCleanupResp>('/admin/conversations/cleanup', { method: 'POST' })
}

export function fetchAdminConversation(requestId: string): Promise<{ data: ArchiveRecord }> {
  return apiFetch<{ data: ArchiveRecord }>(`/admin/logs/${encodeURIComponent(requestId)}/conversation`)
}

export function deleteAdminConversation(requestId: string): Promise<{ ok: boolean; deleted: boolean }> {
  return apiFetch<{ ok: boolean; deleted: boolean }>(`/admin/conversations/${encodeURIComponent(requestId)}`, {
    method: 'DELETE',
  })
}

export function fetchAdminConversations(params: URLSearchParams): Promise<{
  data: ArchiveSummary[]
  backend: 'clickhouse' | 'postgres'
  clickhouse_enabled: boolean
}> {
  const qs = params.toString()
  return apiFetch(`/admin/conversations${qs ? `?${qs}` : ''}`)
}

export function fetchPortalArchivePolicy(): Promise<{ enabled: boolean; allow_user_view: boolean }> {
  return apiFetch('/api/me/conversations/policy')
}

export function fetchPortalConversation(requestId: string): Promise<{ data: ArchiveRecord }> {
  return apiFetch<{ data: ArchiveRecord }>(`/api/me/logs/${encodeURIComponent(requestId)}/conversation`)
}

export function fetchPortalConversations(params: URLSearchParams): Promise<{
  data: ArchiveSummary[]
  backend: 'clickhouse' | 'postgres'
}> {
  const qs = params.toString()
  return apiFetch(`/api/me/conversations${qs ? `?${qs}` : ''}`)
}

function stringifyContent(content: unknown): string {
  if (typeof content === 'string') return content
  if (Array.isArray(content)) {
    const parts: string[] = []
    for (const part of content) {
      if (typeof part === 'string') {
        parts.push(part)
      } else if (part && typeof part === 'object') {
        const rec = part as Record<string, unknown>
        if (typeof rec.text === 'string') {
          parts.push(rec.text)
        } else if (typeof rec.thinking === 'string') {
          parts.push(`[thinking] ${rec.thinking}`)
        } else if (rec.type === 'image_url' && rec.image_url && typeof rec.image_url === 'object') {
          const url = (rec.image_url as Record<string, unknown>).url
          parts.push(typeof url === 'string' ? `[image] ${url}` : '[image]')
        } else if (rec.type === 'image' && rec.source && typeof rec.source === 'object') {
          const data = (rec.source as Record<string, unknown>).data
          parts.push(typeof data === 'string' ? `[image] ${data}` : '[image]')
        } else if (rec.type === 'tool_use') {
          parts.push(`[tool_use ${String(rec.name ?? '')}] ${JSON.stringify(rec.input ?? {})}`)
        } else if (rec.type === 'tool_result') {
          parts.push(`[tool_result] ${stringifyContent(rec.content)}`)
        } else if (rec.inlineData || rec.inline_data) {
          const blob = (rec.inlineData ?? rec.inline_data) as Record<string, unknown>
          parts.push(typeof blob.data === 'string' ? `[media] ${blob.data}` : '[media]')
        } else {
          parts.push(JSON.stringify(rec))
        }
      }
    }
    return parts.filter(Boolean).join('\n')
  }
  if (content && typeof content === 'object') return JSON.stringify(content)
  return ''
}

function normalizeRole(role: unknown): ParsedChatTurn['role'] {
  const s = String(role ?? '').toLowerCase()
  if (s === 'system' || s === 'developer') return 'system'
  if (s === 'assistant' || s === 'model') return 'assistant'
  if (s === 'tool' || s === 'function') return 'tool'
  return 'user'
}

/// 解析多协议（OpenAI Chat / Responses / Anthropic Messages / Gemini）的请求体为统一可读消息列表。
export function parseRequestMessages(rawBody: string): ParsedChatTurn[] {
  let obj: Record<string, unknown>
  try {
    const parsed: unknown = JSON.parse(rawBody)
    if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) return []
    obj = parsed as Record<string, unknown>
  } catch {
    return []
  }

  const turns: ParsedChatTurn[] = []

  // Anthropic `system` / Responses `instructions` / Gemini `systemInstruction`
  if (obj.system !== undefined) {
    const text = stringifyContent(obj.system)
    if (text) turns.push({ role: 'system', text })
  }
  if (typeof obj.instructions === 'string' && obj.instructions.trim() !== '') {
    turns.push({ role: 'system', text: obj.instructions })
  }
  if (obj.systemInstruction && typeof obj.systemInstruction === 'object') {
    const parts = (obj.systemInstruction as Record<string, unknown>).parts
    const text = stringifyContent(parts)
    if (text) turns.push({ role: 'system', text })
  }

  // OpenAI Chat & Anthropic Messages: `messages`
  if (Array.isArray(obj.messages)) {
    for (const msg of obj.messages) {
      if (!msg || typeof msg !== 'object') continue
      const m = msg as Record<string, unknown>
      const text = stringifyContent(m.content)
      const toolCalls = Array.isArray(m.tool_calls)
        ? m.tool_calls.map((tc) => JSON.stringify(tc)).join('\n')
        : ''
      const combined = [text, toolCalls].filter(Boolean).join('\n')
      if (combined) {
        turns.push({
          role: normalizeRole(m.role),
          text: combined,
          ...(typeof m.name === 'string' ? { toolName: m.name } : {}),
        })
      }
    }
    return turns
  }

  // OpenAI Responses: `input`
  if (typeof obj.input === 'string' && obj.input.trim() !== '') {
    turns.push({ role: 'user', text: obj.input })
    return turns
  }
  if (Array.isArray(obj.input)) {
    for (const item of obj.input) {
      if (typeof item === 'string') {
        turns.push({ role: 'user', text: item })
        continue
      }
      if (!item || typeof item !== 'object') continue
      const rec = item as Record<string, unknown>
      if (rec.type === 'function_call_output') {
        turns.push({ role: 'tool', text: stringifyContent(rec.output) })
      } else {
        const text = stringifyContent(rec.content ?? rec.text)
        if (text) turns.push({ role: normalizeRole(rec.role), text })
      }
    }
    return turns
  }

  // Gemini: `contents`
  if (Array.isArray(obj.contents)) {
    for (const item of obj.contents) {
      if (!item || typeof item !== 'object') continue
      const rec = item as Record<string, unknown>
      const text = stringifyContent(rec.parts)
      if (text) turns.push({ role: normalizeRole(rec.role), text })
    }
  }

  return turns
}

export function parseToolCalls(raw: string): ParsedToolCall[] {
  if (!raw || raw.trim() === '') return []
  try {
    const arr: unknown = JSON.parse(raw)
    if (!Array.isArray(arr)) return []
    return arr
      .filter((x): x is Record<string, unknown> => Boolean(x) && typeof x === 'object')
      .map((x) => ({
        id: String(x.id ?? ''),
        name: String(x.name ?? ''),
        arguments: typeof x.arguments === 'string' ? x.arguments : JSON.stringify(x.arguments ?? {}),
      }))
  } catch {
    return []
  }
}

export function prettyJson(raw: string): string {
  if (!raw) return ''
  try {
    return JSON.stringify(JSON.parse(raw), null, 2)
  } catch {
    return raw
  }
}
