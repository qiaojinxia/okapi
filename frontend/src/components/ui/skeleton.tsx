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
export function TableSkeleton({ rows = 6, cols = 5 }: { rows?: number; cols?: number }) {
  return (
    <div className="w-full overflow-hidden rounded-lg border border-border bg-card">
      <div className="flex gap-3 border-b border-border bg-muted/50 px-3 py-2.5">
        {Array.from({ length: cols }).map((_, i) => (
          <Skeleton key={i} className="h-3 flex-1" style={{ maxWidth: i === 0 ? 48 : 140 }} />
        ))}
      </div>
      {Array.from({ length: rows }).map((_, r) => (
        <div key={r} className="flex items-center gap-3 border-b border-border px-3 py-3 last:border-b-0">
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
export function StatSkeleton() {
  return (
    <div className="flex items-start gap-3 rounded-lg border border-border bg-card p-4 shadow-card">
      <Skeleton className="h-9 w-9 rounded-md" />
      <div className="flex flex-1 flex-col gap-2 pt-0.5">
        <Skeleton className="h-3 w-20" />
        <Skeleton className="h-5 w-28" />
        <Skeleton className="h-3 w-32" />
      </div>
    </div>
  )
}
