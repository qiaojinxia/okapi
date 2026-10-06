import { useCallback, useEffect, useRef, useState } from 'react'
import { streamChat } from './chat-stream'
import type { ChatParams, ChatUsage, ToolCallDelta } from './chat-stream'
import { toWireMessages } from './messages'
import { getKey } from '@/lib/api'

/// 模型发起的一次工具调用。`result` 是用户回填的执行结果；有值才算"已回填"。
export interface ToolCall {
  id: string
  name: string
  /// 参数 JSON 文本（流式期间是残缺的）。
  arguments: string
  result?: string
}

/// 对话里的一条（助手条带流式状态与脚注）。
export interface Turn {
  id: number
  role: 'user' | 'assistant'
  content: string
  reasoning?: string
  /// 模型在这一轮发起的工具调用（按 `index` 顺序）。
  toolCalls?: ToolCall[]
  /// 请求时填的模型（别名）；`model` 才是上游实际应答的那个，估价按请求的别名查价目。
  requested?: string
  /// 这次请求生效的价目分组（选了密钥时是该密钥的分组），估价按它取倍率。
  group?: string
  /// 选用的密钥名（登录会话为空）：对话里看得出这条是用哪把令牌调的。
  via?: string
  /// 上游实际服务的模型（可能与请求的别名不同）。
  model?: string | null
  usage?: ChatUsage | null
  /// 从发出到第一个字（正文或推理）到达的毫秒数；整条回复收尾的总毫秒数。
  ttftMs?: number
  durationMs?: number
  /// 后端 error_code（i18n 在 errors 命名空间渲染）；有值 = 这条回复失败。
  error?: { code: string; status: number; param?: string; message?: string }
  streaming?: boolean
}

export interface SendOptions {
  model: string
  system: string
  temperature: number | null
  top_p: number | null
  max_tokens: number | null
  reasoning_effort?: string | null
  thinking_budget?: number | null
  preserve_reasoning?: boolean
  /// 工具定义（已校验的 OpenAI `tools` 数组）；空 = 不声明工具。
  tools?: unknown[] | null
  /// 选用的密钥：`keyId` 为空 = 登录会话；`group` 是该次请求生效的价目分组。
  keyId: number | null
  keyName: string | null
  group: string
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

/// 把一块里的工具调用增量并进已有调用：同一 `index` 按序拼接。
/// `id` / `name` 只在缺失时才采纳（有的上游每块都重发，不能叠加）；参数文本一律追加。
export function mergeToolCalls(current: ToolCall[] | undefined, deltas: ToolCallDelta[]): ToolCall[] | undefined {
  if (deltas.length === 0) return current
  const calls = [...(current ?? [])]
  for (const delta of deltas) {
    while (calls.length <= delta.index) calls.push({ id: '', name: '', arguments: '' })
    const call = calls[delta.index]
    calls[delta.index] = {
      ...call,
      id: call.id || delta.id || '',
      name: call.name || delta.name || '',
      arguments: call.arguments + delta.arguments,
    }
  }
  return calls
}

/// 收尾：丢掉没有名字的空洞，给缺 id 的调用补一个稳定 id（回填结果时按 id 配对，id 必须唯一非空）。
function settleToolCalls(calls: ToolCall[] | undefined, turnId: number): ToolCall[] | undefined {
  const settled = (calls ?? []).map((call, index) => ({ ...call, id: call.id || `call_${turnId}_${index}` })).filter((call) => call.name !== '')
  return settled.length > 0 ? settled : undefined
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

  /// 在 `base` 之后发起流式请求；`text` 非空时先追加一条用户消息，为 null 时是"工具结果回填后的续写"
  /// （上一轮 assistant 的工具调用已带结果，直接让模型接着答）。`send` / `regenerate` / 回填结果共用。
  const run = useCallback(
    (base: Turn[], text: string | null, opts: SendOptions) => {
      const key = getKey()
      if (key === null || busy || (text !== null && text.trim() === '') || opts.model.trim() === '') return
      const messages: ChatParams['messages'] = [
        ...(opts.system.trim() !== '' ? [{ role: 'system' as const, content: opts.system.trim() }] : []),
        ...toWireMessages(base, { model: opts.model.trim(), preserveReasoning: opts.preserve_reasoning }),
        ...(text !== null ? [{ role: 'user' as const, content: text }] : []),
      ]
      const params: ChatParams = {
        model: opts.model.trim(),
        messages,
        ...(opts.temperature !== null ? { temperature: opts.temperature } : {}),
        ...(opts.top_p !== null ? { top_p: opts.top_p } : {}),
        ...(opts.max_tokens !== null ? { max_tokens: opts.max_tokens } : {}),
        ...(opts.reasoning_effort ? { reasoning_effort: opts.reasoning_effort } : {}),
        ...(opts.thinking_budget != null ? { reasoning: { max_tokens: opts.thinking_budget } } : {}),
        ...(opts.tools && opts.tools.length > 0 ? { tools: opts.tools } : {}),
      }
      const userTurn: Turn | null = text === null ? null : { id: nextId.current++, role: 'user', content: text }
      const assistantTurn: Turn = { id: nextId.current++, role: 'assistant', content: '', requested: params.model, group: opts.group, via: opts.keyName ?? undefined, streaming: true }
      setTurns([...base, ...(userTurn ? [userTurn] : []), assistantTurn])
      setBusy(true)
      const startedAt = performance.now()
      abortRef.current = streamChat(params, key, {
        onDelta: (d) =>
          patchLast((t) => ({
            ...t,
            content: t.content + d.content,
            reasoning: d.reasoning ? (t.reasoning ?? '') + d.reasoning : t.reasoning,
            toolCalls: mergeToolCalls(t.toolCalls, d.toolCalls),
            model: t.model ?? d.model,
            usage: d.usage ?? t.usage,
            ttftMs: t.ttftMs ?? (d.content !== '' || d.reasoning !== '' || d.toolCalls.length > 0 ? Math.round(performance.now() - startedAt) : undefined),
          })),
        onDone: () => {
          patchLast((t) => ({ ...t, streaming: false, toolCalls: settleToolCalls(t.toolCalls, t.id), durationMs: Math.round(performance.now() - startedAt) }))
          abortRef.current = null
          setBusy(false)
        },
        onError: (code, status, param, message) => {
          patchLast((t) => ({ ...t, streaming: false, toolCalls: settleToolCalls(t.toolCalls, t.id), error: { code, status, param, message } }))
          abortRef.current = null
          setBusy(false)
        },
      }, opts.keyId)
    },
    [busy, patchLast],
  )

  const send = useCallback((text: string, opts: SendOptions) => run(turns, text, opts), [run, turns])

  /// 重新生成最后一条回复：用同样的上文、当前参数再来一次。失败的回复也走这里（"重试"）。
  /// 上文是用户消息时，等于把那句话重新问一遍；上文是已回填结果的工具调用时，只重做续写。
  const regenerate = useCallback(
    (opts: SendOptions) => {
      const last = turns.length - 1
      if (last < 1 || turns[last].role !== 'assistant') return
      const before = turns[last - 1]
      if (before.role === 'user') run(turns.slice(0, last - 1), before.content, opts)
      else run(turns.slice(0, last), null, opts)
    },
    [run, turns],
  )

  /// 回填工具结果并让模型继续：`results` 按调用 id 给出，写进那一轮的 `toolCalls`，随后发起续写。
  /// 只接受"最后一条 assistant 回合"——更早的回合改了结果会让后文失效。
  const submitToolResults = useCallback(
    (turnId: number, results: Record<string, string>, opts: SendOptions) => {
      const at = turns.findIndex((t) => t.id === turnId)
      if (at < 0 || at !== turns.length - 1) return
      const filled = turns.map((t) => (t.id === turnId
        ? { ...t, toolCalls: t.toolCalls?.map((call) => ({ ...call, result: results[call.id] ?? call.result })) }
        : t))
      run(filled, null, opts)
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

  return { turns, busy, send, regenerate, submitToolResults, stop, clear }
}
