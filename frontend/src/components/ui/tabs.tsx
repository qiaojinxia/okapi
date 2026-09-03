import type { LucideIcon } from 'lucide-react'
import { cn } from '@/lib/utils'

export interface TabItem {
  id: string
  label: string
  icon?: LucideIcon
  /// 右侧计数徽章（待处理条数等）。
  count?: number
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
  className,
}: {
  items: TabItem[]
  active: string
  onChange: (id: string) => void
  variant?: 'pill' | 'underline'
  className?: string
}) {
  if (variant === 'underline') {
    return (
      <div
        role="tablist"
        className={cn(
          'flex max-w-full items-center gap-1 overflow-x-auto border-b border-border scrollbar-none',
          className,
        )}
      >
        {items.map((item) => {
          const on = active === item.id
          return (
            <button
              key={item.id}
              type="button"
              role="tab"
              aria-selected={on}
              onClick={() => onChange(item.id)}
              className={cn(
                '-mb-px inline-flex shrink-0 items-center gap-1.5 border-b-2 px-3 py-2 text-sm whitespace-nowrap transition-colors outline-none',
                'focus-visible:rounded-sm focus-visible:ring-2 focus-visible:ring-primary/40',
                on
                  ? 'border-primary font-medium text-foreground'
                  : 'border-transparent text-muted-foreground hover:border-border hover:text-foreground',
              )}
            >
              {item.icon && <item.icon className="h-4 w-4" />}
              {item.label}
              {item.count !== undefined && <TabCount n={item.count} active={on} />}
            </button>
          )
        })}
      </div>
    )
  }
  return (
    <div
      role="tablist"
      className={cn(
        'inline-flex max-w-full items-center gap-0.5 self-start overflow-x-auto rounded-lg border border-border bg-muted/60 p-0.5 scrollbar-none',
        className,
      )}
    >
      {items.map((item) => {
        const on = active === item.id
        return (
          <button
            key={item.id}
            type="button"
            role="tab"
            aria-selected={on}
            className={cn(
              'inline-flex h-8 shrink-0 items-center gap-1.5 rounded-md px-3 text-sm whitespace-nowrap transition-all outline-none',
              'focus-visible:ring-2 focus-visible:ring-primary/40',
              on
                ? 'bg-card font-medium text-foreground shadow-card'
                : 'text-muted-foreground hover:text-foreground',
            )}
            onClick={() => onChange(item.id)}
          >
            {item.icon && <item.icon className="h-4 w-4" />}
            {item.label}
            {item.count !== undefined && <TabCount n={item.count} active={on} />}
          </button>
        )
      })}
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
