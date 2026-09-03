import { useEffect, useRef, useState } from 'react'
import { createPortal } from 'react-dom'
import { cn } from '@/lib/utils'

interface TooltipProps {
  /// 提示文字；空串 = 不显示。
  content: string
  side?: 'top' | 'bottom'
  /// 触发元素；需能接受 mouse/focus 事件（普通 DOM 元素或透传 props 的组件）。
  children: React.ReactElement
  className?: string
}

/// 轻量 tooltip（无依赖，portal 到 body）。
///
/// 替代原生 `title`：原生提示要停留近 1s 才出现、样式不可控、暗色下是白底黑字，
/// 表格里一排图标按钮全靠它辨认动作，慢一拍就会点错。portal 而非就地渲染是因为
/// 表格容器 `overflow-auto` 会把上方弹出的提示裁掉。
/// 纯视觉层（aria-hidden）：可访问名字仍由触发元素自己的 aria-label 提供。
export function Tooltip({ content, side = 'top', children, className }: TooltipProps) {
  const anchor = useRef<HTMLSpanElement>(null)
  const [pos, setPos] = useState<{ x: number; y: number } | null>(null)

  const show = () => {
    const el = anchor.current
    if (!el || content === '') return
    const r = el.getBoundingClientRect()
    setPos({ x: r.left + r.width / 2, y: side === 'top' ? r.top : r.bottom })
  }
  const hide = () => setPos(null)

  // 滚动/缩放时直接收起而不是跟随：跟随要监听一堆事件，收起更稳
  useEffect(() => {
    if (pos === null) return undefined
    window.addEventListener('scroll', hide, true)
    window.addEventListener('resize', hide)
    return () => {
      window.removeEventListener('scroll', hide, true)
      window.removeEventListener('resize', hide)
    }
  }, [pos])

  return (
    <>
      <span
        ref={anchor}
        className={cn('inline-flex', className)}
        onMouseEnter={show}
        onMouseLeave={hide}
        onFocus={show}
        onBlur={hide}
      >
        {children}
      </span>
      {pos !== null &&
        createPortal(
          <span
            aria-hidden
            className={cn(
              'pointer-events-none fixed z-[80] max-w-xs -translate-x-1/2 rounded-md bg-foreground px-2 py-1 text-xs font-medium whitespace-nowrap text-background shadow-popover animate-fade-in',
              side === 'top' ? '-translate-y-full' : '',
            )}
            style={{
              left: pos.x,
              top: side === 'top' ? pos.y - 6 : pos.y + 6,
            }}
          >
            {content}
          </span>,
          document.body,
        )}
    </>
  )
}
