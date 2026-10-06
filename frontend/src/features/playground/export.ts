import { prettyArguments } from './tools'
import type { Turn } from './use-chat-stream'

/// 围栏代码块：围栏比正文里出现的最长反引号串多一个，内容里有 ``` 也不会提前闭合。
function fenced(text: string, lang = ''): string {
  const longest = Math.max(0, ...(text.match(/`+/g) ?? []).map((run) => run.length))
  const fence = '`'.repeat(Math.max(3, longest + 1))
  return `${fence}${lang}\n${text}\n${fence}`
}

/// 对话导出为 Markdown：角色标题 + 原文，助手回复末尾留一行模型与用量；工具调用写成"名称 + 参数 + 返回结果"。
/// 标签由调用方经 t() 传入（本文件不产生文案）。失败的回复与空回复不导出。
export function conversationMarkdown(turns: Turn[], labels: { title: string; user: string; assistant: string; toolCall: string; toolResult: string; usage: (turn: Turn) => string }): string {
  const parts = [`# ${labels.title}`]
  for (const turn of turns) {
    const calls = turn.toolCalls ?? []
    if (turn.error !== undefined || (turn.role === 'assistant' && turn.content === '' && calls.length === 0)) continue
    parts.push(`## ${turn.role === 'user' ? labels.user : labels.assistant}`)
    if (turn.content !== '') parts.push(turn.content)
    for (const call of calls) {
      parts.push(`### ${labels.toolCall}: ${call.name}`, fenced(prettyArguments(call.arguments).text || '{}', 'json'))
      if (call.result !== undefined) parts.push(`**${labels.toolResult}**`, fenced(call.result))
    }
    if (turn.role === 'assistant') {
      const footer = labels.usage(turn)
      if (footer !== '') parts.push(`> ${footer}`)
    }
  }
  return `${parts.join('\n\n')}\n`
}

/// 触发浏览器下载一个文本文件。
export function downloadText(filename: string, text: string, mime = 'text/markdown'): void {
  const url = URL.createObjectURL(new Blob([text], { type: `${mime};charset=utf-8` }))
  const a = document.createElement('a')
  a.href = url
  a.download = filename
  document.body.append(a)
  a.click()
  a.remove()
  URL.revokeObjectURL(url)
}
