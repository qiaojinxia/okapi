import { Check, Copy } from 'lucide-react'
import { useCallback, useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { toast } from '@/components/ui/toast'
import { Tooltip } from '@/components/ui/tooltip'
import { cn } from '@/lib/utils'

/// 复制到剪贴板并给出反馈。
///
/// 此前五处 `navigator.clipboard.writeText` 都是点了没任何回应——用户不知道复制成没成，
/// 往往连点三次再去粘贴。返回值 `copied` 供按钮切换成对勾 1.5s。
export function useCopy(): { copied: boolean; copy: (text: string) => Promise<void> } {
  const { t } = useTranslation()
  const [copied, setCopied] = useState(false)
  const timer = useRef<number | null>(null)
  useEffect(
    () => () => {
      if (timer.current !== null) window.clearTimeout(timer.current)
    },
    [],
  )
  const copy = useCallback(
    async (text: string) => {
      try {
        await navigator.clipboard.writeText(text)
        setCopied(true)
        toast.success(t('common:copied'))
        if (timer.current !== null) window.clearTimeout(timer.current)
        timer.current = window.setTimeout(() => setCopied(false), 1_500)
      } catch {
        toast.error(t('common:copyFailed'))
      }
    },
    [t],
  )
  return { copied, copy }
}

interface CopyButtonProps {
  value: string
  /// tooltip / 可访问名字；缺省"复制"。
  label?: string
  size?: 'xs' | 'sm'
  className?: string
}

/// 图标态复制按钮：贴在 request_id / 订单号 / 密钥旁边。
export function CopyButton({ value, label, size = 'sm', className }: CopyButtonProps) {
  const { t } = useTranslation()
  const { copied, copy } = useCopy()
  const name = label ?? t('common:copy')
  const dim = size === 'xs' ? 'h-6 w-6' : 'h-7 w-7'
  const icon = size === 'xs' ? 'h-3 w-3' : 'h-3.5 w-3.5'
  return (
    <Tooltip content={copied ? t('common:copied') : name}>
      <button
        type="button"
        aria-label={name}
        onClick={(e) => {
          e.stopPropagation()
          void copy(value)
        }}
        className={cn(
          'inline-flex shrink-0 items-center justify-center rounded-md text-muted-foreground transition-colors outline-none',
          'hover:bg-muted hover:text-foreground focus-visible:ring-2 focus-visible:ring-primary/40',
          dim,
          className,
        )}
      >
        {copied ? <Check className={cn(icon, 'text-success')} /> : <Copy className={icon} />}
      </button>
    </Tooltip>
  )
}

/// 可复制的等宽文本（ID / 前缀 / 链接）：文本 + 复制按钮成组。
export function CopyText({
  value,
  display,
  className,
  mono = true,
}: {
  value: string
  /// 展示文本（如截短的 ID）；缺省显示完整值。
  display?: React.ReactNode
  className?: string
  mono?: boolean
}) {
  return (
    <span className={cn('inline-flex min-w-0 items-center gap-1', className)}>
      <span className={cn('truncate', mono && 'font-mono text-xs')} title={value}>
        {display ?? value}
      </span>
      <CopyButton value={value} size="xs" />
    </span>
  )
}
