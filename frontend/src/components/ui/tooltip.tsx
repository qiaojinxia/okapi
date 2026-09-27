import { cloneElement, useEffect, useId, useLayoutEffect, useRef, useState } from 'react'
import type { ReactElement } from 'react'
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
/// 长说明与触发控件关联；靠近视口边缘时换行、翻转并限制位置。
export function Tooltip({ content, side = 'top', children, className }: TooltipProps) {
  const anchor = useRef<HTMLSpanElement>(null)
  const popup = useRef<HTMLSpanElement>(null)
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null)
  const hovered = useRef(false)
  const id = useId()
  const [open, setOpen] = useState(false)
  const visible = open && content !== ''
  const cancelClose = () => { if (timer.current !== null) clearTimeout(timer.current); timer.current = null }
  const show = () => { cancelClose(); if (content !== '') setOpen(true) }
  const hide = () => { cancelClose(); setOpen(false) }
  const leave = () => {
    cancelClose()
    timer.current = setTimeout(() => {
      if (!hovered.current && !anchor.current?.contains(document.activeElement)) setOpen(false)
    }, 120)
  }
  useEffect(() => () => { if (timer.current !== null) clearTimeout(timer.current) }, [])

  useLayoutEffect(() => {
    const tip = popup.current, trigger = anchor.current
    if (!visible || !tip || !trigger) return
    let frame = 0, previous = ''
    const place = () => {
      const viewport = window.visualViewport
      const left = (viewport?.offsetLeft ?? 0) + 8, top = (viewport?.offsetTop ?? 0) + 8
      const width = (viewport?.width ?? window.innerWidth) - 16
      const height = (viewport?.height ?? window.innerHeight) - 16
      tip.style.maxWidth = `${Math.max(0, Math.min(320, width))}px`
      tip.style.maxHeight = `${Math.max(0, height)}px`
      const target = trigger.getBoundingClientRect(), box = tip.getBoundingClientRect()
      const above = target.top - top - 6, below = top + height - target.bottom - 6
      const onTop = side === 'top' ? above >= box.height || above >= below : below < box.height && above > below
      const x = Math.max(left, Math.min(target.left + (target.width - box.width) / 2, left + width - box.width))
      const y = Math.max(top, Math.min(onTop ? target.top - box.height - 6 : target.bottom + 6, top + height - box.height))
      const next = `${x}:${y}`
      if (next !== previous) { tip.style.left = `${x}px`; tip.style.top = `${y}px`; previous = next }
      tip.style.visibility = 'visible'
      // 抽屉入场可能改变锚点位置而不改变大小。
      frame = requestAnimationFrame(place)
    }
    place()
    return () => cancelAnimationFrame(frame)
  }, [visible, content, side])

  // 页面滚动、离开控件或 Escape 收起；提示本身可悬停阅读。
  useEffect(() => {
    if (!visible) return
    const scroll = (e: Event) => { if (!popup.current?.contains(e.target as Node)) hide() }
    const outside = (e: PointerEvent) => { if (!anchor.current?.contains(e.target as Node) && !popup.current?.contains(e.target as Node)) hide() }
    const escape = (e: KeyboardEvent) => {
      if (e.key !== 'Escape' || e.isComposing || e.defaultPrevented) return
      e.preventDefault()
      e.stopPropagation()
      hide()
    }
    window.addEventListener('scroll', scroll, true)
    window.addEventListener('resize', hide)
    document.addEventListener('pointerdown', outside)
    document.addEventListener('keydown', escape, true)
    return () => {
      window.removeEventListener('scroll', scroll, true)
      window.removeEventListener('resize', hide)
      document.removeEventListener('pointerdown', outside)
      document.removeEventListener('keydown', escape, true)
    }
  }, [visible])

  const child = children as ReactElement<{ 'aria-label'?: string; 'aria-describedby'?: string }>
  const trigger = visible && child.props['aria-label'] !== content
    ? cloneElement(child, { 'aria-describedby': [child.props['aria-describedby'], id].filter(Boolean).join(' ') }) : children

  return (
    <>
      <span
        ref={anchor}
        className={cn('inline-flex', className)}
        onMouseEnter={() => { hovered.current = true; show() }}
        onMouseLeave={() => { hovered.current = false; leave() }}
        onFocus={show}
        onBlur={leave}
      >
        {trigger}
      </span>
      {visible &&
        createPortal(
          <span
            id={id} ref={popup} role="tooltip"
            onMouseEnter={() => { hovered.current = true; cancelClose() }}
            onMouseLeave={() => { hovered.current = false; leave() }}
            className="fixed z-[80] w-max max-w-xs overflow-auto overscroll-contain rounded-md bg-foreground px-2.5 py-1.5 text-xs leading-5 font-medium whitespace-normal text-background shadow-popover [overflow-wrap:anywhere]"
            style={{ visibility: 'hidden', left: 0, top: 0 }}
          >
            {content}
          </span>,
          document.body,
        )}
    </>
  )
}
