import type { LucideIcon } from 'lucide-react'
import { useEffect, useId, useRef, useState } from 'react'
import { cn } from '@/lib/utils'

export interface TabItem {
  id: string
  label: string
  icon?: LucideIcon
  /// 右侧计数徽章（待处理条数等）。
  count?: number
  panelId?: string
}

/// 页签：把一屏里并列的多块内容拆成"一次只看一块"。
///
/// 用在两类地方：多张数据卡纵向堆叠的页面（统计/运维——五张卡全挂会一次发五组
/// 查询且滚动无尽头），以及分段过多的编辑抽屉（渠道——六段表单滚起来找不到北）。
/// 只做受控组件：激活态由调用方持有，便于"仅挂载激活面板"以省掉隐藏面板的请求。
///
/// 两种形态：`pill`（缺省，灰底白块）用在卡片/抽屉内；`underline`（下划线）用在
/// 页级内容切换，与页头同宽时更像"本页的分区"而不是一个控件。
/// 窄屏下横向滚动而非折行：折成两行的页签栏读不出哪个是当前。
export function Tabs({
  items,
  active,
  onChange,
  variant = 'pill',
  id,
  ariaLabel,
  className,
}: {
  items: TabItem[]
  active: string
  onChange: (id: string) => void
  variant?: 'pill' | 'underline'
  id?: string
  ariaLabel?: string
  className?: string
}) {
  const [focused, setFocused] = useState(active)
  const generatedId = useId()
  const tabsId = id ?? generatedId
  const root = useRef<HTMLDivElement>(null)
  const entry = items.some((item) => item.id === focused)
    ? focused
    : items.find((item) => item.id === active)?.id ?? items[0]?.id
  const underline = variant === 'underline'

  useEffect(() => { setFocused(active) }, [active])

  useEffect(() => {
    const list = root.current
    if (!list) return
    const reveal = () => {
      const focusedTab = document.activeElement instanceof HTMLElement && document.activeElement.getAttribute('role') === 'tab' && list.contains(document.activeElement)
        ? document.activeElement : list.querySelector<HTMLElement>('[aria-selected=true]')
      if (focusedTab) revealTab(list, focusedTab)
    }
    reveal()
    // 缩窗或横竖屏切换后，当前页签仍应留在可见范围内。
    const observer = new ResizeObserver(reveal)
    observer.observe(list)
    return () => observer.disconnect()
  }, [active])

  return (
    <div
      ref={root}
      id={tabsId}
      role="tablist"
      aria-label={ariaLabel}
      aria-orientation="horizontal"
      onBlur={(e) => {
        if (!e.currentTarget.contains(e.relatedTarget)) setFocused(active)
      }}
      className={cn(
        'max-w-full items-center overflow-x-auto scrollbar-none',
        underline
          ? 'flex gap-1 border-b border-border'
          : 'inline-flex gap-0.5 self-start rounded-lg border border-border bg-muted/60 p-0.5',
        className,
      )}
    >
      {items.map((item, index) => {
        const on = active === item.id
        return (
          <button
            key={item.id}
            type="button"
            role="tab"
            id={`${tabsId}-${item.id}`}
            aria-controls={item.panelId}
            aria-selected={on}
            tabIndex={entry === item.id ? 0 : -1}
            onFocus={(e) => {
              setFocused(item.id)
              // 只滚动页签条，避免长表单或页面跟着跳动。
              const list = root.current
              if (!list) return
              revealTab(list, e.currentTarget)
            }}
            onKeyDown={(e) => {
              if (e.altKey || e.ctrlKey || e.metaKey) return
              const next = e.key === 'Home' ? 0
                : e.key === 'End' ? items.length - 1
                : e.key === 'ArrowRight' ? (index + 1) % items.length
                : e.key === 'ArrowLeft' ? (index - 1 + items.length) % items.length
                : null
              if (next === null) return
              e.preventDefault()
              // 手动激活：方向键仅定位，Enter / Space 交给原生按钮触发切换。
              root.current?.querySelectorAll<HTMLButtonElement>('[role=tab]')[next]?.focus({ preventScroll: true })
            }}
            className={cn(
              'inline-flex min-h-11 shrink-0 items-center gap-1.5 px-3 text-sm whitespace-nowrap transition-colors outline-none md:min-h-8',
              'focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-primary/60',
              underline
                ? cn('-mb-px border-b-2 py-2', on
                    ? 'border-primary font-medium text-foreground'
                    : 'border-transparent text-muted-foreground hover:border-border hover:text-foreground')
                : cn('rounded-md', on
                    ? 'bg-card font-medium text-foreground shadow-card'
                    : 'text-muted-foreground hover:text-foreground'),
            )}
            onClick={() => { if (!on) onChange(item.id) }}
          >
            {item.icon && <item.icon aria-hidden className="h-4 w-4" />}
            {item.label}
            {item.count !== undefined && <TabCount n={item.count} active={on} />}
          </button>
        )
      })}
    </div>
  )
}

function revealTab(list: HTMLElement, tab: HTMLElement) {
  const rect = tab.getBoundingClientRect()
  const bounds = list.getBoundingClientRect()
  if (rect.left < bounds.left) list.scrollLeft += rect.left - bounds.left
  else if (rect.right > bounds.right) list.scrollLeft += rect.right - bounds.right
}

/// 表单分区首次访问才挂载，之后切签只隐藏，保留尚未保存的输入。
export function TabPanel({ id, labelledBy, active, children }: {
  id: string
  labelledBy: string
  active: boolean
  children: React.ReactNode
}) {
  const [visited, setVisited] = useState(active)
  useEffect(() => { if (active) setVisited(true) }, [active])
  return (
    <div id={id} role="tabpanel" aria-labelledby={labelledBy} hidden={!active} tabIndex={0}>
      {(active || visited) && children}
    </div>
  )
}

function TabCount({ n, active }: { n: number; active: boolean }) {
  return (
    <span
      className={cn(
        'rounded-full px-1.5 py-px text-[10px] font-semibold tabular-nums',
        active ? 'bg-primary/10 text-primary' : 'bg-muted text-muted-foreground',
      )}
    >
      {n}
    </span>
  )
}
