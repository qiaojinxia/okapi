import type { LucideIcon } from 'lucide-react'
import { cloneElement, createContext, useContext, useEffect, useId, useRef, useState } from 'react'
import { cn } from '@/lib/utils'

interface MenuCtx {
  close: () => void
}
const Ctx = createContext<MenuCtx>({ close: () => undefined })

interface MenuProps {
  /// 单个按钮或透传原生按钮 props 的组件（如 Button）。
  trigger: React.ReactElement<React.ButtonHTMLAttributes<HTMLButtonElement>>
  align?: 'start' | 'end'
  /// 弹层最小宽度类，如 `min-w-56`。
  className?: string
  children: React.ReactNode
}

/// 下拉菜单（无依赖）：点击切换、外点关闭、Esc 关闭、选中后自动关闭。
///
/// 用在顶栏身份菜单与表格行的"更多"动作——五个以上的行动作平铺成一排图标已经
/// 认不出谁是谁，折进菜单给文字。方向键定位，Enter / Space 选择，Esc 返回触发器。
export function Menu({ trigger, align = 'end', className, children }: MenuProps) {
  const [open, setOpen] = useState(false)
  const root = useRef<HTMLDivElement>(null)
  const panel = useRef<HTMLDivElement>(null)
  const startAtEnd = useRef(false)
  const id = useId()
  const focusTrigger = () => root.current?.querySelector<HTMLButtonElement>('button')?.focus({ preventScroll: true })
  const close = () => {
    focusTrigger()
    setOpen(false)
  }

  useEffect(() => {
    if (!open) return undefined
    const items = panel.current?.querySelectorAll<HTMLButtonElement>('[role=menuitem]:not(:disabled)')
    const first = startAtEnd.current ? items?.[items.length - 1] : items?.[0]
    ;(first ?? panel.current)?.focus({ preventScroll: true })
    const onDown = (e: PointerEvent) => {
      if (root.current && !root.current.contains(e.target as Node)) setOpen(false)
    }
    document.addEventListener('pointerdown', onDown)
    return () => {
      document.removeEventListener('pointerdown', onDown)
    }
  }, [open])

  return (
    <div
      ref={root}
      className="relative inline-flex"
      onBlur={(e) => {
        if (!e.currentTarget.contains(e.relatedTarget)) setOpen(false)
      }}
    >
      {cloneElement(trigger, {
        id: trigger.props.id ?? `${id}-trigger`,
        'aria-haspopup': 'menu',
        'aria-expanded': open,
        'aria-controls': open ? id : undefined,
        onClick: (e) => {
          trigger.props.onClick?.(e)
          if (e.defaultPrevented) return
          startAtEnd.current = false
          setOpen((v) => !v)
        },
        onKeyDown: (e) => {
          trigger.props.onKeyDown?.(e)
          if (e.defaultPrevented || (e.key !== 'ArrowDown' && e.key !== 'ArrowUp')) return
          e.preventDefault()
          startAtEnd.current = e.key === 'ArrowUp'
          setOpen(true)
        },
      })}
      {open && (
        <Ctx.Provider value={{ close }}>
          <div
            ref={panel}
            id={id}
            role="menu"
            aria-labelledby={trigger.props.id ?? `${id}-trigger`}
            tabIndex={-1}
            onKeyDown={(e) => {
              if (e.key === 'Escape') {
                e.preventDefault()
                e.stopPropagation()
                close()
                return
              }
              if (e.key === 'Tab') {
                // 从触发器继续自然 Tab 顺序；Shift+Tab 则回到触发器。
                if (e.shiftKey) e.preventDefault()
                close()
                return
              }
              const items = [...e.currentTarget.querySelectorAll<HTMLButtonElement>('[role=menuitem]:not(:disabled)')]
              if (items.length === 0) return
              const index = items.indexOf(document.activeElement as HTMLButtonElement)
              const next = e.key === 'Home' ? 0
                : e.key === 'End' ? items.length - 1
                : e.key === 'ArrowDown' ? (index + 1) % items.length
                : e.key === 'ArrowUp' ? (index - 1 + items.length) % items.length
                : null
              if (next === null) return
              e.preventDefault()
              items[next]?.focus({ preventScroll: true })
            }}
            className={cn(
              'absolute top-full z-50 mt-1.5 flex min-w-48 flex-col gap-0.5 rounded-lg border border-border bg-popover p-1 shadow-popover animate-zoom-in',
              align === 'end' ? 'right-0 origin-top-right' : 'left-0 origin-top-left',
              className,
            )}
          >
            {children}
          </div>
        </Ctx.Provider>
      )}
    </div>
  )
}

export function MenuItem({
  icon: Icon,
  children,
  onSelect,
  destructive = false,
  disabled = false,
  /// 右侧附注（快捷键 / 当前值）。
  trailing,
}: {
  icon?: LucideIcon
  children: React.ReactNode
  onSelect?: () => void
  destructive?: boolean
  disabled?: boolean
  trailing?: React.ReactNode
}) {
  const { close } = useContext(Ctx)
  return (
    <button
      type="button"
      role="menuitem"
      tabIndex={-1}
      disabled={disabled}
      onClick={() => {
        close()
        onSelect?.()
      }}
      className={cn(
        'flex min-h-11 w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-sm outline-none transition-colors md:min-h-8',
        'focus-visible:bg-accent disabled:pointer-events-none disabled:opacity-50',
        destructive ? 'text-destructive hover:bg-destructive/10' : 'hover:bg-accent',
      )}
    >
      {Icon && <Icon className="h-4 w-4 shrink-0 opacity-80" />}
      <span className="flex-1 truncate">{children}</span>
      {trailing !== undefined && (
        <span className="text-xs text-muted-foreground">{trailing}</span>
      )}
    </button>
  )
}

export function MenuLabel({ children }: { children: React.ReactNode }) {
  return (
    <div className="px-2 pt-1.5 pb-1 text-[11px] font-medium tracking-wide text-muted-foreground uppercase">
      {children}
    </div>
  )
}

export function MenuSeparator() {
  return <div role="separator" className="my-1 h-px bg-border" />
}
