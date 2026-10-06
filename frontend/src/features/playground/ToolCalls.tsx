import { ChevronRight, Send, Wrench } from 'lucide-react'
import { useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { prettyArguments } from './tools'
import type { ToolCall } from './use-chat-stream'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { CopyButton } from '@/components/ui/copy-button'
import { Textarea } from '@/components/ui/textarea'

/// 助手回复里的工具调用：一个调用一张可折叠卡片（与"推理过程"同为折叠展示）。
///
/// 折叠态只露出函数名 + 参数一行预览 + 状态；展开才是缩进后的参数 JSON（可复制）与返回结果。
/// 还在等结果的调用在流式结束后自动展开（等着用户填），填完提交即折叠；用户手动开合不被打断。
/// `onSubmit` 只在"这是最后一条助手回复且当前可发送"时传入——更早的回合改结果会让后文失效。
export function ToolCallList({ calls, streaming, onSubmit }: {
  calls: ToolCall[]
  streaming: boolean
  onSubmit?: (results: Record<string, string>) => void
}) {
  const { t } = useTranslation()
  const [results, setResults] = useState<Record<string, string>>({})
  const respondable = onSubmit !== undefined && !streaming
  const missing = calls.filter((call) => call.result === undefined && (results[call.id] ?? '').trim() === '').length
  const waiting = respondable && calls.some((call) => call.result === undefined)
  return (
    <div className="mt-2 space-y-1.5 first:mt-0" data-slot="tool-calls">
      {calls.map((call) => (
        <ToolCallCard
          key={call.id || call.name}
          call={call}
          streaming={streaming}
          respondable={respondable}
          value={results[call.id] ?? ''}
          onValue={(value) => setResults((previous) => ({ ...previous, [call.id]: value }))}
        />
      ))}
      {waiting && (
        <div className="flex flex-wrap items-center justify-between gap-2 pt-0.5">
          <span className="text-xs text-muted-foreground" role="status">{missing > 0 ? t('portal:playgroundToolWaiting', { n: missing }) : ''}</span>
          <Button size="sm" disabled={missing > 0} onClick={() => onSubmit?.(results)}>
            <Send className="h-3.5 w-3.5" />
            {t('portal:playgroundToolSubmit')}
          </Button>
        </div>
      )}
    </div>
  )
}

function ToolCallCard({ call, streaming, respondable, value, onValue }: {
  call: ToolCall
  streaming: boolean
  respondable: boolean
  value: string
  onValue: (value: string) => void
}) {
  const { t } = useTranslation()
  const pending = call.result === undefined
  const [open, setOpen] = useState(false)
  // 等结果且已生成完：展开让用户填；有了结果 / 又在生成：折叠。用户中途手动开合不受影响（只在这两个量变化时重算）
  useEffect(() => { setOpen(pending && !streaming && respondable) }, [pending, streaming, respondable])
  const args = prettyArguments(call.arguments)
  const preview = call.arguments.replace(/\s+/g, ' ').trim()
  const status = streaming ? t('portal:playgroundToolGenerating') : !pending ? t('portal:playgroundToolDone') : respondable ? t('portal:playgroundToolPending') : t('portal:playgroundToolSkipped')
  return (
    <details
      open={open}
      onToggle={(event) => setOpen(event.currentTarget.open)}
      data-slot="tool-call"
      data-state={streaming ? 'streaming' : pending ? 'pending' : 'done'}
      className="group rounded-lg border border-border bg-background/70 text-xs"
    >
      <summary className="flex cursor-pointer list-none items-center gap-2 rounded-lg px-2.5 py-1.5 outline-none focus-visible:ring-2 focus-visible:ring-primary/40 [&::-webkit-details-marker]:hidden">
        <ChevronRight aria-hidden className="h-3 w-3 shrink-0 text-muted-foreground transition-transform group-open:rotate-90" />
        <Wrench aria-hidden className="h-3.5 w-3.5 shrink-0 text-primary" />
        <span className="shrink-0 font-mono font-medium text-foreground">{call.name}</span>
        <span className="min-w-0 flex-1 truncate font-mono text-muted-foreground" title={preview}>{preview === '' && !streaming ? t('portal:playgroundToolEmptyArgs') : preview}</span>
        <Badge variant={streaming ? 'muted' : pending ? (respondable ? 'warning' : 'muted') : 'success'} className="shrink-0">{status}</Badge>
      </summary>
      <div className="space-y-2 border-t border-border/70 p-2.5">
        <div>
          <div className="mb-1 flex items-center justify-between">
            <span className="font-medium text-muted-foreground">{t('portal:playgroundToolArgs')}</span>
            {args.text !== '' && <CopyButton value={args.text} label={t('portal:playgroundToolCopyArgs')} size="xs" />}
          </div>
          <pre className="max-h-64 overflow-auto rounded-md bg-muted/50 p-2 font-mono leading-5 whitespace-pre-wrap break-all" data-slot="tool-call-args">{args.text === '' ? t('portal:playgroundToolEmptyArgs') : args.text}</pre>
        </div>
        {!pending && (
          <div>
            <span className="mb-1 block font-medium text-muted-foreground">{t('portal:playgroundToolResult')}</span>
            <pre className="max-h-64 overflow-auto rounded-md bg-muted/50 p-2 font-mono leading-5 whitespace-pre-wrap break-all" data-slot="tool-call-result">{call.result}</pre>
          </div>
        )}
        {pending && respondable && (
          <div>
            <span className="mb-1 block font-medium text-muted-foreground">{t('portal:playgroundToolResult')}</span>
            <Textarea
              rows={3}
              className="font-mono text-xs"
              value={value}
              aria-label={t('portal:playgroundToolResultFor', { name: call.name })}
              placeholder={t('portal:playgroundToolResultPlaceholder')}
              onChange={(event) => onValue(event.target.value)}
            />
          </div>
        )}
      </div>
    </details>
  )
}
