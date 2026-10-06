import type { ChatMessage } from './chat-stream'
import type { Turn } from './use-chat-stream'

/// 把对话回合还原成发给上游的 `messages`。
///
/// 工具调用必须"成对"：assistant 的 `tool_calls` 后面要跟着每个调用各一条 `tool` 消息，
/// 否则 OpenAI 系上游直接 400。所以只有**全部回填了结果**的那一轮才带 `tool_calls`；
/// 还没回填就继续聊的，这一轮退化成纯文本（没有文本就整条略去），保证请求始终合法。
/// 失败的回复不进历史。推理文本只在上游声明要保留（`preserveReasoning`）且是同一个模型时回传。
export function toWireMessages(turns: Turn[], opts: { model: string; preserveReasoning?: boolean }): ChatMessage[] {
  const out: ChatMessage[] = []
  for (const turn of turns) {
    if (turn.error !== undefined) continue
    if (turn.role === 'user') {
      out.push({ role: 'user', content: turn.content })
      continue
    }
    const reasoning = opts.preserveReasoning && turn.requested === opts.model && typeof turn.reasoning === 'string' ? { reasoning_content: turn.reasoning } : {}
    const calls = (turn.toolCalls ?? []).filter((call) => call.name !== '')
    if (calls.length > 0 && calls.every((call) => call.result !== undefined)) {
      out.push({
        role: 'assistant',
        content: turn.content === '' ? null : turn.content,
        ...reasoning,
        tool_calls: calls.map((call) => ({ id: call.id, type: 'function' as const, function: { name: call.name, arguments: call.arguments } })),
      })
      for (const call of calls) out.push({ role: 'tool', tool_call_id: call.id, content: call.result ?? '' })
    } else if (turn.content !== '') {
      out.push({ role: 'assistant', content: turn.content, ...reasoning })
    }
  }
  return out
}
