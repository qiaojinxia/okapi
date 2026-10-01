import type { Turn } from './use-chat-stream'

/// 对话导出为 Markdown：角色标题 + 原文，助手回复末尾留一行模型与用量。
/// 标签由调用方经 t() 传入（本文件不产生文案）。失败的回复与空回复不导出。
export function conversationMarkdown(turns: Turn[], labels: { title: string; user: string; assistant: string; usage: (turn: Turn) => string }): string {
  const parts = [`# ${labels.title}`]
  for (const turn of turns) {
    if (turn.error !== undefined || (turn.role === 'assistant' && turn.content === '')) continue
    parts.push(`## ${turn.role === 'user' ? labels.user : labels.assistant}`, turn.content)
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
