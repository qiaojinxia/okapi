import type { ReactNode } from 'react'
import { useTranslation } from 'react-i18next'
import { CopyButton } from '@/components/ui/copy-button'

/// 助手回复的轻量 markdown 渲染（不引入 markdown 库，也不用 `dangerouslySetInnerHTML`）。
///
/// 模型回复里最常见的就是代码块、列表、加粗、行内代码、链接和表格；其余语法按原文显示即可。
/// 全部输出 React 元素，文本由 React 转义；链接只放行 http(s) / mailto，杜绝 `javascript:`。
/// 流式期间正文是残缺的：未闭合的代码围栏按"代码一直延续到末尾"处理，下一块到达再续上。

export type Block =
  | { kind: 'code'; lang: string; text: string }
  | { kind: 'heading'; level: number; text: string }
  | { kind: 'list'; ordered: boolean; items: string[] }
  | { kind: 'quote'; text: string }
  | { kind: 'table'; head: string[]; rows: string[][] }
  | { kind: 'rule' }
  | { kind: 'para'; text: string }

const FENCE = /^ {0,3}(`{3,}|~{3,})\s*([\w+#.-]*)[^`]*$/
const HEADING = /^ {0,3}(#{1,6})\s+(.*?)(?:\s+#+)?\s*$/
const RULE = /^ {0,3}([-*_])(?:\s*\1){2,}\s*$/
const LIST = /^(\s*)([-*+]|\d{1,9}[.)])\s+(.*)$/
const QUOTE = /^ {0,3}>\s?(.*)$/
const TABLE_SEP = /^\s*\|?\s*:?-{2,}:?\s*(\|\s*:?-{2,}:?\s*)*\|?\s*$/

const splitRow = (line: string): string[] => line.trim().replace(/^\|/, '').replace(/\|$/, '').split('|').map((cell) => cell.trim())

/// 把一段 markdown 切成块。纯函数，便于单测。
export function parseBlocks(source: string): Block[] {
  const lines = source.replace(/\r\n?/g, '\n').split('\n')
  const blocks: Block[] = []
  let para: string[] = []
  const flush = () => {
    if (para.length > 0) blocks.push({ kind: 'para', text: para.join('\n') })
    para = []
  }
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i]
    const fence = FENCE.exec(line)
    if (fence) {
      flush()
      const marker = fence[1]
      const body: string[] = []
      i++
      while (i < lines.length && !(lines[i].trimStart().startsWith(marker) && lines[i].trim().replace(new RegExp(`^${marker[0]}+`), '') === '')) body.push(lines[i++])
      blocks.push({ kind: 'code', lang: fence[2], text: body.join('\n') })
      continue
    }
    if (line.trim() === '') {
      flush()
      continue
    }
    const heading = HEADING.exec(line)
    if (heading) {
      flush()
      blocks.push({ kind: 'heading', level: heading[1].length, text: heading[2] })
      continue
    }
    if (RULE.test(line)) {
      flush()
      blocks.push({ kind: 'rule' })
      continue
    }
    if (line.includes('|') && i + 1 < lines.length && TABLE_SEP.test(lines[i + 1]) && lines[i + 1].includes('-')) {
      flush()
      const head = splitRow(line)
      const rows: string[][] = []
      i += 2
      while (i < lines.length && lines[i].includes('|') && lines[i].trim() !== '') rows.push(splitRow(lines[i++]))
      i--
      blocks.push({ kind: 'table', head, rows })
      continue
    }
    const quote = QUOTE.exec(line)
    if (quote) {
      flush()
      const body = [quote[1]]
      while (i + 1 < lines.length && QUOTE.test(lines[i + 1])) body.push(QUOTE.exec(lines[++i])![1])
      blocks.push({ kind: 'quote', text: body.join('\n') })
      continue
    }
    const item = LIST.exec(line)
    if (item) {
      flush()
      const ordered = /\d/.test(item[2])
      const items = [item[3]]
      // 缩进的后续行并入上一条；遇到同类标记开新条；空行或其他块结束列表
      while (i + 1 < lines.length) {
        const next = lines[i + 1]
        const nextItem = LIST.exec(next)
        if (nextItem && /\d/.test(nextItem[2]) === ordered) {
          items.push(nextItem[3])
        } else if (/^\s+\S/.test(next) && !nextItem) {
          items[items.length - 1] += `\n${next.trim()}`
        } else break
        i++
      }
      blocks.push({ kind: 'list', ordered, items })
      continue
    }
    para.push(line)
  }
  flush()
  return blocks
}

const INLINE = new RegExp(
  [
    '`([^`\\n]+)`', // 1 行内代码
    '\\*\\*([^\\s*](?:[^*]*[^\\s*])?)\\*\\*', // 2 加粗
    '\\*([^\\s*](?:[^*\\n]*[^\\s*])?)\\*', // 3 斜体（不处理下划线：snake_case 标识符太常见）
    '\\[([^\\]\\n]+)\\]\\((https?:\\/\\/[^\\s)]+|mailto:[^\\s)]+)\\)', // 4 文字 5 地址
    '(https?:\\/\\/[^\\s<>]*[^\\s<>.,;:!?)"\'\\]，。；：！？）])', // 6 裸链接
  ].join('|'),
  'g',
)

const LINK_CLASS = 'text-primary underline underline-offset-2 break-all hover:opacity-80'

/// 行内语法：行内代码 / 加粗 / 斜体 / 链接。代码内不再解析；加粗、斜体、链接文字可嵌套。
export function renderInline(text: string, keyPrefix = 'i'): ReactNode[] {
  const out: ReactNode[] = []
  let last = 0
  let n = 0
  for (const m of text.matchAll(INLINE)) {
    const index = m.index ?? 0
    if (index > last) out.push(text.slice(last, index))
    const key = `${keyPrefix}-${n++}`
    if (m[1] !== undefined) out.push(<code key={key} className="rounded bg-foreground/10 px-1 py-0.5 font-mono text-[0.85em]">{m[1]}</code>)
    else if (m[2] !== undefined) out.push(<strong key={key} className="font-semibold">{renderInline(m[2], key)}</strong>)
    else if (m[3] !== undefined) out.push(<em key={key}>{renderInline(m[3], key)}</em>)
    else if (m[4] !== undefined) out.push(<a key={key} href={m[5]} target="_blank" rel="noopener noreferrer" className={LINK_CLASS}>{renderInline(m[4], key)}</a>)
    else out.push(<a key={key} href={m[6]} target="_blank" rel="noopener noreferrer" className={LINK_CLASS}>{m[6]}</a>)
    last = index + m[0].length
  }
  if (last < text.length) out.push(text.slice(last))
  return out
}

function CodeBlock({ lang, text }: { lang: string; text: string }) {
  const { t } = useTranslation()
  return (
    <div className="my-2 overflow-hidden rounded-lg border border-border bg-muted/50 text-foreground first:mt-0 last:mb-0">
      <div className="flex items-center justify-between border-b border-border/70 bg-muted/60 py-0.5 pl-3 pr-1">
        <span className="font-mono text-[11px] text-muted-foreground">{lang || t('portal:playgroundCodeText')}</span>
        <CopyButton value={text} label={t('portal:playgroundCopyCode')} size="xs" />
      </div>
      <pre className="overflow-x-auto p-3 font-mono text-xs leading-5"><code>{text}</code></pre>
    </div>
  )
}

const HEADING_SIZE = ['text-base', 'text-base', 'text-sm', 'text-sm', 'text-sm', 'text-sm']

/// 渲染一整段回复。每个块各占一个元素，块间距由 `space-y` 统一。
export function Markdown({ source }: { source: string }) {
  const blocks = parseBlocks(source)
  return (
    <div className="space-y-2 break-words [overflow-wrap:anywhere]">
      {blocks.map((block, index) => {
        const key = `b${index}`
        switch (block.kind) {
          case 'code':
            return <CodeBlock key={key} lang={block.lang} text={block.text} />
          case 'heading':
            return <p key={key} className={`pt-1 font-semibold ${HEADING_SIZE[block.level - 1]}`}>{renderInline(block.text, key)}</p>
          case 'rule':
            return <hr key={key} className="border-border" />
          case 'quote':
            return <blockquote key={key} className="whitespace-pre-wrap border-l-2 border-border pl-3 text-muted-foreground">{renderInline(block.text, key)}</blockquote>
          case 'list': {
            const List = block.ordered ? 'ol' : 'ul'
            return (
              <List key={key} className={`space-y-1 pl-5 ${block.ordered ? 'list-decimal' : 'list-disc'}`}>
                {block.items.map((item, i) => <li key={i} className="whitespace-pre-wrap pl-0.5">{renderInline(item, `${key}-${i}`)}</li>)}
              </List>
            )
          }
          case 'table':
            return (
              <div key={key} className="overflow-x-auto rounded-lg border border-border">
                <table className="w-full border-collapse text-xs">
                  <thead className="bg-muted/60">
                    <tr>{block.head.map((cell, i) => <th key={i} scope="col" className="border-b border-border px-2.5 py-1.5 text-left font-semibold">{renderInline(cell, `${key}-h${i}`)}</th>)}</tr>
                  </thead>
                  <tbody>
                    {block.rows.map((row, r) => (
                      <tr key={r} className="border-b border-border/60 last:border-0">
                        {block.head.map((_, c) => <td key={c} className="px-2.5 py-1.5 align-top">{renderInline(row[c] ?? '', `${key}-${r}-${c}`)}</td>)}
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )
          default:
            return <p key={key} className="whitespace-pre-wrap">{renderInline(block.text, key)}</p>
        }
      })}
    </div>
  )
}
