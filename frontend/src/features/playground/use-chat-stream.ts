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
  /// 上游实际服务的模型（可能与请求的别名不同）。
  model?: string | null
  usage?: ChatUsage | null
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

/// Playground 对话状态机：发送 → 流式追加 → 完成 / 失败 / 中断。
/// 组件不碰 fetch / ReadableStream（frontend.mdc："SSE 封装专用 hook"）。
export function useChatStream() {
  const [turns, setTurns] = useState<Turn[]>([])
  const [busy, setBusy] = useState(false)
  const abortRef = useRef<(() => void) | null>(null)
  const nextId = useRef(1)

  // 卸载时掐掉在途流：离开页面不该让请求继续跑（它已经在计费了，但至少不再拉字节）
  useEffect(() => () => abortRef.current?.(), [])

  const patchLast = useCallback((fn: (t: Turn) => Turn) => {
    setTurns((prev) => {
      if (prev.length === 0) return prev
      const last = prev[prev.length - 1]
      return [...prev.slice(0, -1), fn(last)]
    })
  }, [])

  const send = useCallback(
    (text: string, opts: SendOptions) => {
      const key = getKey()
      if (key === null || busy || text.trim() === '' || opts.model.trim() === '') return
      const history: ChatMessage[] = turns
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
      const assistantTurn: Turn = { id: nextId.current++, role: 'assistant', content: '', streaming: true }
      setTurns((prev) => [...prev, userTurn, assistantTurn])
      setBusy(true)
      abortRef.current = streamChat(params, key, {
        onDelta: (d) =>
          patchLast((t) => ({
            ...t,
            content: t.content + d.content,
            reasoning: d.reasoning ? (t.reasoning ?? '') + d.reasoning : t.reasoning,
            model: t.model ?? d.model,
            usage: d.usage ?? t.usage,
          })),
        onDone: () => {
          patchLast((t) => ({ ...t, streaming: false }))
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
    [busy, turns, patchLast],
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

  return { turns, busy, send, stop, clear }
}
