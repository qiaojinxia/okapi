import { cn } from '@/lib/utils'

/// 骨架屏底块。加载时给出"这里会出现什么形状"，比一个居中的旋转图标更少跳动：
/// 数据回来时版面不会从 40px 高突然长到 400px。
export function Skeleton({ className, ...props }: React.HTMLAttributes<HTMLDivElement>) {
  return (
    <div
      aria-hidden
      className={cn('rounded-md skeleton-shimmer animate-shimmer', className)}
      {...props}
    />
  )
}

/// 表格骨架：与真实表格同一外框，行数按每页大小给，切页时表格高度不抖。
export function TableSkeleton({ rows = 6, cols = 5, dense = false }: { rows?: number; cols?: number; dense?: boolean }) {
  return (
    <div data-slot="table-skeleton" data-density={dense ? 'compact' : 'default'} aria-busy="true" className="w-full overflow-hidden rounded-xl border border-border bg-card shadow-card">
      <div className="flex h-[var(--table-header-height)] items-center gap-3 border-b border-border bg-muted px-3">
        {Array.from({ length: cols }).map((_, i) => (
          <Skeleton key={i} className="h-3 flex-1" style={{ maxWidth: i === 0 ? 48 : 140 }} />
        ))}
      </div>
      {Array.from({ length: rows }).map((_, r) => (
        <div key={r} className="flex h-[var(--table-row-height)] items-center gap-3 border-b border-border px-3 last:border-b-0">
          {Array.from({ length: cols }).map((_, c) => (
            <Skeleton
              key={c}
              className="h-3.5 flex-1"
              style={{ maxWidth: c === 0 ? 48 : 120 + ((r * 37 + c * 53) % 80) }}
            />
          ))}
        </div>
      ))}
    </div>
  )
}

/// KPI 卡骨架：与 `Stat` 同尺寸。
export function StatSkeleton({ layout = 'inline', compact = false, className, icon = true, sub = true }: {
  layout?: 'inline' | 'stacked'
  compact?: boolean
  className?: string
  icon?: boolean
  sub?: boolean
}) {
  return (
    <div aria-hidden className={cn('relative flex min-w-0 items-start gap-3 rounded-lg border border-border bg-card p-4 shadow-card', compact && 'p-3', className)}>
      {icon && <Skeleton className={cn('h-9 w-9 shrink-0 rounded-md', layout === 'stacked' && 'absolute right-4 top-3 h-7 w-7')} />}
      <div className="flex min-w-0 flex-1 flex-col gap-2 pt-0.5">
        <div className={cn(layout === 'stacked' && 'min-h-7 pr-8', compact && layout === 'stacked' && 'min-h-5')}><Skeleton className="h-3 w-full max-w-20" /></div>
        <Skeleton className="h-5 w-full max-w-28" />
        {sub && <Skeleton className="h-3 w-full max-w-32" />}
      </div>
    </div>
  )
}
