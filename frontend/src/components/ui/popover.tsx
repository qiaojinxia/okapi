import { cloneElement, useCallback, useEffect, useId, useLayoutEffect, useRef, useState } from 'react'
import type { ButtonHTMLAttributes, ReactElement, ReactNode } from 'react'
import { createPortal } from 'react-dom'
import { cn } from '@/lib/utils'

type Place = { left: number; top?: number; bottom?: number; maxHeight: number }

interface PopoverProps {
  /// 单个按钮元素（或透传原生按钮 props 的组件）：点击切换弹出框。
  trigger: ReactElement<ButtonHTMLAttributes<HTMLButtonElement>>
  /// 弹出框的可访问名称（`role="dialog"` 的 aria-label）。
  label: string
  open: boolean
  onOpenChange: (open: boolean) => void
  /// 与触发器哪一侧对齐；窗口放不下时会被夹回可视区域内。
  align?: 'start' | 'end'
  className?: string
  /// 内容；给函数时拿到 `close`（关闭并把焦点还给触发按钮）。
  children: ReactNode | ((api: { close: () => void }) => ReactNode)
}

/// 弹出框：触发按钮 + 锚在它下方的浮层，用来放"选个值再确定"这类不该撑开页面的小表单
/// （自定义日期、时间范围）。
///
/// - 浮层渲染到 `body` 并用 fixed 定位：不会被表格 / 卡片的 `overflow` 裁掉，也不受祖先
///   `transform` 影响。下方放不下而上方更宽裕就翻到上方，横向夹在窗口内；页面滚动、窗口缩放、
///   浮层自身变高（月历 5 行 ↔ 6 行）时跟着重新定位，而不是收起——里面有输入框，手机上弹出键盘
///   就会触发缩放与滚动，一滚就关会让人没法输入。触发器暂时滚出窗口也不收起（窗口缩放的瞬间、整页截图
///   都会冒出临时尺寸）：滚回来浮层还在原位。
/// - `Esc` 关闭并把焦点还给触发按钮；点浮层外面、Tab 离开浮层也关闭（不抢焦点）。
/// - 打开后焦点落在 `data-autofocus` 的元素上，没有就落在第一个可聚焦元素上（触屏落在浮层本身）。
export function Popover({ trigger, label, open, onOpenChange, align = 'start', className, children }: PopoverProps) {
  const id = useId()
  const anchor = useRef<HTMLSpanElement>(null)
  const panel = useRef<HTMLDivElement>(null)
  const place = useAnchoredPosition(open, anchor, panel, align)

  const close = useCallback(() => {
    onOpenChange(false)
    anchor.current?.querySelector<HTMLElement>('button')?.focus({ preventScroll: true })
  }, [onOpenChange])

  // 打开且定好位之后把焦点送进浮层；关闭时不动（Esc / 完成按钮走 close() 还焦点，
  // 点外面则让点中的元素拿焦点）
  const positioned = place !== null
  useEffect(() => {
    if (!open || !positioned) return
    const el = panel.current
    if (!el || el.contains(document.activeElement)) return
    // 触屏上不自动聚焦输入框：会弹出软键盘 / 日期滚轮盖住浮层，焦点只进浮层本身
    const touch = window.matchMedia?.('(pointer: coarse)').matches === true
    const target =
      el.querySelector<HTMLElement>('[data-autofocus]') ??
      (touch ? null : el.querySelector<HTMLElement>('input:not(:disabled), select:not(:disabled), button:not(:disabled), [tabindex]:not([tabindex="-1"])'))
    ;(target ?? el).focus({ preventScroll: true })
  }, [open, positioned])

  useEffect(() => {
    if (!open) return undefined
    const inside = (node: EventTarget | null) =>
      node instanceof Node && (panel.current?.contains(node) === true || anchor.current?.contains(node) === true)
    const onPointerDown = (event: PointerEvent) => {
      if (!inside(event.target)) onOpenChange(false)
    }
    const onFocusIn = (event: FocusEvent) => {
      if (!inside(event.target)) onOpenChange(false)
    }
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== 'Escape' || event.defaultPrevented) return
      if (!inside(document.activeElement) && document.activeElement !== document.body) return
      event.preventDefault()
      event.stopPropagation()
      close()
    }
    document.addEventListener('pointerdown', onPointerDown)
    document.addEventListener('focusin', onFocusIn)
    document.addEventListener('keydown', onKeyDown, true)
    return () => {
      document.removeEventListener('pointerdown', onPointerDown)
      document.removeEventListener('focusin', onFocusIn)
      document.removeEventListener('keydown', onKeyDown, true)
    }
  }, [open, onOpenChange, close])

  return (
    <>
      <span ref={anchor} className="inline-flex max-w-full min-w-0">
        {cloneElement(trigger, {
          'aria-haspopup': 'dialog',
          'aria-expanded': open,
          'aria-controls': open ? id : undefined,
          onClick: (event) => {
            trigger.props.onClick?.(event)
            if (!event.defaultPrevented) onOpenChange(!open)
          },
        })}
      </span>
      {open &&
        createPortal(
          <div
            ref={panel}
            id={id}
            role="dialog"
            aria-label={label}
            tabIndex={-1}
            data-slot="popover"
            // portal 里的 React 事件仍会沿组件树冒泡：浮层里表单的 submit 不能把外层表单（如分析页的
            // 高级筛选）也一起提交了
            onSubmit={(event) => event.stopPropagation()}
            // 还没算出位置的那一刻用透明而不是 visibility:hidden：后者会被子元素继承，减弱动效偏好下
            // （全局把 transition 缩成 0.01ms）每个后代都要“过渡”一帧才变可见，这一帧里 focus() 会静默失败
            style={place === null ? { opacity: 0, pointerEvents: 'none' } : place}
            className={cn(
              'fixed z-50 w-max max-w-[calc(100vw-1rem)] overflow-y-auto overscroll-contain rounded-xl border border-border bg-popover p-3 text-popover-foreground shadow-popover outline-none transition-none animate-zoom-in',
              className,
            )}
          >
            {typeof children === 'function' ? children({ close }) : children}
          </div>,
          document.body,
        )}
    </>
  )
}

/// 浮层相对触发器的位置：打开时在绘制前算好（不闪），之后随滚动 / 缩放 / 浮层变高重算。
function useAnchoredPosition(
  open: boolean,
  anchor: React.RefObject<HTMLElement | null>,
  panel: React.RefObject<HTMLElement | null>,
  align: 'start' | 'end',
): Place | null {
  const [place, setPlace] = useState<Place | null>(null)

  const update = useCallback(() => {
    const trigger = anchor.current?.getBoundingClientRect()
    const el = panel.current
    if (!trigger || !el) return
    const gap = 6
    const margin = 8
    // offsetWidth / offsetHeight 是布局尺寸，不受入场动画 transform 影响
    const width = el.offsetWidth
    const height = el.offsetHeight
    const below = window.innerHeight - trigger.bottom - gap - margin
    const above = trigger.top - gap - margin
    const flip = height > below && above > below
    const left = Math.max(margin, Math.min(align === 'end' ? trigger.right - width : trigger.left, window.innerWidth - width - margin))
    const next: Place = flip
      ? { left, bottom: window.innerHeight - trigger.top + gap, maxHeight: Math.max(160, above) }
      : { left, top: trigger.bottom + gap, maxHeight: Math.max(160, below) }
    setPlace((prev) =>
      prev !== null && prev.left === next.left && prev.top === next.top && prev.bottom === next.bottom && prev.maxHeight === next.maxHeight ? prev : next,
    )
  }, [anchor, panel, align])

  useLayoutEffect(() => {
    if (!open) {
      setPlace(null)
      return
    }
    update()
  }, [open, update])

  useEffect(() => {
    if (!open) return undefined
    let frame = 0
    const schedule = () => {
      cancelAnimationFrame(frame)
      frame = requestAnimationFrame(update)
    }
    window.addEventListener('resize', schedule)
    window.addEventListener('scroll', schedule, true)
    window.visualViewport?.addEventListener('resize', schedule)
    window.visualViewport?.addEventListener('scroll', schedule)
    const observer = new ResizeObserver(schedule)
    if (panel.current) observer.observe(panel.current)
    return () => {
      cancelAnimationFrame(frame)
      window.removeEventListener('resize', schedule)
      window.removeEventListener('scroll', schedule, true)
      window.visualViewport?.removeEventListener('resize', schedule)
      window.visualViewport?.removeEventListener('scroll', schedule)
      observer.disconnect()
    }
  }, [open, update, panel])

  return place
}
