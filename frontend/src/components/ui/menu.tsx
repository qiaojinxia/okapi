import type { LucideIcon } from 'lucide-react'
import { createContext, useContext, useEffect, useRef, useState } from 'react'
import { cn } from '@/lib/utils'

interface MenuCtx {
  close: () => void
}
const Ctx = createContext<MenuCtx>({ close: () => undefined })

interface MenuProps {
  /// 触发器：会被包在一个 span 里接管点击；触发器自己不要再绑 onClick。
  trigger: React.ReactNode
  align?: 'start' | 'end'
  /// 弹层最小宽度类，如 `min-w-56`。
  className?: string
  children: React.ReactNode
}

/// 下拉菜单（无依赖）：点击切换、外点关闭、Esc 关闭、选中后自动关闭。
///
/// 用在顶栏身份菜单与表格行的"更多"动作——五个以上的行动作平铺成一排图标已经
/// 认不出谁是谁，折进菜单给文字。不做复杂的键盘漫游，Tab 顺序按 DOM 即可。
export function Menu({ trigger, align = 'end', className, children }: MenuProps) {
  const [open, setOpen] = useState(false)
  const root = useRef<HTMLDivElement>(null)

  useEffect(() => {
    if (!open) return undefined
    const onDown = (e: MouseEvent) => {
      if (root.current && !root.current.contains(e.target as Node)) setOpen(false)
    }
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setOpen(false)
    }
    document.addEventListener('mousedown', onDown)
    document.addEventListener('keydown', onKey)
    return () => {
      document.removeEventListener('mousedown', onDown)
      document.removeEventListener('keydown', onKey)
    }
  }, [open])

  return (
    <div ref={root} className="relative inline-flex">
      <span
        className="inline-flex"
        onClick={() => setOpen((v) => !v)}
        aria-haspopup="menu"
        aria-expanded={open}
      >
        {trigger}
      </span>
      {open && (
        <Ctx.Provider value={{ close: () => setOpen(false) }}>
          <div
            role="menu"
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
      disabled={disabled}
      onClick={() => {
        onSelect?.()
        close()
      }}
      className={cn(
        'flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-sm outline-none transition-colors',
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
