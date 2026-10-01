import { useCallback, useEffect, useRef, useState } from 'react'
import { streamChat } from './chat-stream'
import type { ChatMessage, ChatParams, ChatUsage } from './chat-stream'
import { getKey } from '@/lib/api'

/// 对话里的一条（助手条带流式状态与脚注）。
export interface Turn {
  id: number
  role: 'user' | 'assistant'
  content: string
  reasoning?: string
  /// 请求时填的模型（别名）；`model` 才是上游实际应答的那个，估价按请求的别名查价目。
  requested?: string
  /// 上游实际服务的模型（可能与请求的别名不同）。
  model?: string | null
  usage?: ChatUsage | null
  /// 从发出到第一个字（正文或推理）到达的毫秒数；整条回复收尾的总毫秒数。
  ttftMs?: number
  durationMs?: number
  /// 后端 error_code（i18n 在 errors 命名空间渲染）；有值 = 这条回复失败。
  error?: { code: string; status: number; param?: string }
  streaming?: boolean
}

export interface SendOptions {
  model: string
  system: string
  temperature: number
  top_p: number
  max_tokens: number | null
}

// 对话按用户存在 sessionStorage：离开试用台去模型广场看价、再回来，不该丢掉聊了一半的内容；
// 关掉标签页即清空（对话里可能有敏感内容，不进 localStorage）。
const MAX_STORED_TURNS = 60
const MAX_STORED_CHARS = 400_000
const storageKey = (userId: number) => `okapi.playground.chat.${userId}`

function isTurn(v: unknown): v is Turn {
  if (typeof v !== 'object' || v === null) return false
  const t = v as Record<string, unknown>
  return typeof t.id === 'number' && (t.role === 'user' || t.role === 'assistant') && typeof t.content === 'string'
}

export function readTurns(userId: number | undefined): Turn[] {
  if (userId === undefined) return []
  try {
    const raw = JSON.parse(sessionStorage.getItem(storageKey(userId)) ?? '[]') as unknown
    // 上次是在流式途中离开的：残缺的回复保留原样，但不能再显示"生成中"
    return Array.isArray(raw) ? raw.filter(isTurn).map((t) => (t.streaming ? { ...t, streaming: false } : t)) : []
  } catch {
    return []
  }
}

function writeTurns(userId: number, turns: Turn[]): void {
  try {
    const kept = turns.slice(-MAX_STORED_TURNS)
    const text = JSON.stringify(kept)
    if (turns.length === 0 || text.length > MAX_STORED_CHARS) sessionStorage.removeItem(storageKey(userId))
    else sessionStorage.setItem(storageKey(userId), text)
  } catch {
    // 存储不可用（隐私模式 / 配额）：只是不保留，不影响对话本身
  }
}

/// Playground 对话状态机：发送 → 流式追加 → 完成 / 失败 / 中断；可重新生成最后一条回复。
/// 组件不碰 fetch / ReadableStream（frontend.mdc："SSE 封装专用 hook"）。
export function useChatStream(userId?: number) {
  const [turns, setTurns] = useState<Turn[]>(() => readTurns(userId))
  const [busy, setBusy] = useState(false)
  const abortRef = useRef<(() => void) | null>(null)
  const nextId = useRef(turns.reduce((max, t) => Math.max(max, t.id), 0) + 1)

  // 卸载时掐掉在途流：离开页面不该让请求继续跑（它已经在计费了，但至少不再拉字节）
  useEffect(() => () => abortRef.current?.(), [])

  // 流式期间每个 delta 都改 turns，不逐块写盘；收尾（busy 回落）后落一次
  useEffect(() => {
    if (!busy && userId !== undefined) writeTurns(userId, turns)
  }, [busy, turns, userId])

  const patchLast = useCallback((fn: (t: Turn) => Turn) => {
    setTurns((prev) => {
      if (prev.length === 0) return prev
      const last = prev[prev.length - 1]
      return [...prev.slice(0, -1), fn(last)]
    })
  }, [])

  /// 在 `base` 之后追加一条用户消息并发起流式请求。`send` 与 `regenerate` 共用。
  const run = useCallback(
    (base: Turn[], text: string, opts: SendOptions) => {
      const key = getKey()
      if (key === null || busy || text.trim() === '' || opts.model.trim() === '') return
      const history: ChatMessage[] = base
        .filter((t) => t.error === undefined && (t.role === 'user' || t.content !== ''))
        .map((t) => ({ role: t.role, content: t.content }))
      const messages: ChatMessage[] = [
        ...(opts.system.trim() !== '' ? [{ role: 'system' as const, content: opts.system.trim() }] : []),
        ...history,
        { role: 'user', content: text },
      ]
      const params: ChatParams = {
        model: opts.model.trim(),
        messages,
        temperature: opts.temperature,
        top_p: opts.top_p,
        ...(opts.max_tokens !== null ? { max_tokens: opts.max_tokens } : {}),
      }
      const userTurn: Turn = { id: nextId.current++, role: 'user', content: text }
      const assistantTurn: Turn = { id: nextId.current++, role: 'assistant', content: '', requested: params.model, streaming: true }
      setTurns([...base, userTurn, assistantTurn])
      setBusy(true)
      const startedAt = performance.now()
      abortRef.current = streamChat(params, key, {
        onDelta: (d) =>
          patchLast((t) => ({
            ...t,
            content: t.content + d.content,
            reasoning: d.reasoning ? (t.reasoning ?? '') + d.reasoning : t.reasoning,
            model: t.model ?? d.model,
            usage: d.usage ?? t.usage,
            ttftMs: t.ttftMs ?? (d.content !== '' || d.reasoning !== '' ? Math.round(performance.now() - startedAt) : undefined),
          })),
        onDone: () => {
          patchLast((t) => ({ ...t, streaming: false, durationMs: Math.round(performance.now() - startedAt) }))
          abortRef.current = null
          setBusy(false)
        },
        onError: (code, status, param) => {
          patchLast((t) => ({ ...t, streaming: false, error: { code, status, param } }))
          abortRef.current = null
          setBusy(false)
        },
      })
    },
    [busy, patchLast],
  )

  const send = useCallback((text: string, opts: SendOptions) => run(turns, text, opts), [run, turns])

  /// 重新生成：丢掉最后一条用户消息之后的内容，用同一句话、当前参数再问一次。
  /// 失败的回复也走这里（"重试"）。
  const regenerate = useCallback(
    (opts: SendOptions) => {
      const at = turns.map((t) => t.role).lastIndexOf('user')
      if (at < 0) return
      run(turns.slice(0, at), turns[at].content, opts)
    },
    [run, turns],
  )

  const stop = useCallback(() => {
    abortRef.current?.()
    abortRef.current = null
    patchLast((t) => ({ ...t, streaming: false }))
    setBusy(false)
  }, [patchLast])

  const clear = useCallback(() => {
    abortRef.current?.()
    abortRef.current = null
    setTurns([])
    setBusy(false)
  }, [])

  return { turns, busy, send, regenerate, stop, clear }
}
