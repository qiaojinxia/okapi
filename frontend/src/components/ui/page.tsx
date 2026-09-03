import type { LucideIcon } from 'lucide-react'
import { cn } from '@/lib/utils'

/// 页头：标题 + 这一页负责什么 + 主操作（通常是"新建"）。
///
/// 一页一职责的前提是用户能一眼知道这页管什么。此前列表页直接甩出表格，
/// 页面之间只靠侧栏高亮区分，同屏还混着别的资源的表单。
export function PageHeader({
  title,
  description,
  icon: Icon,
  meta,
  action,
  className,
}: {
  title: string
  description?: string
  icon?: LucideIcon
  /// 标题右侧的小徽章（计数 / 状态）。
  meta?: React.ReactNode
  action?: React.ReactNode
  className?: string
}) {
  return (
    <header className={cn('flex flex-wrap items-start justify-between gap-3', className)}>
      <div className="flex min-w-0 items-start gap-3">
        {Icon && (
          <span className="mt-0.5 hidden h-9 w-9 shrink-0 items-center justify-center rounded-lg bg-primary/10 text-primary sm:flex">
            <Icon className="h-4.5 w-4.5" />
          </span>
        )}
        <div className="flex min-w-0 flex-col gap-1">
          <div className="flex flex-wrap items-center gap-2">
            <h1 className="text-xl font-semibold tracking-tight">{title}</h1>
            {meta}
          </div>
          {description !== undefined && (
            <p className="max-w-3xl text-sm leading-5 text-muted-foreground">{description}</p>
          )}
        </div>
      </div>
      {action !== undefined && <div className="flex flex-wrap items-center gap-2">{action}</div>}
    </header>
  )
}

/// 工具栏：搜索/过滤在左，右侧放计数或次级动作。
///
/// 批量操作不再放这里——选中若干条后由底部浮起的 `SelectionBar` 承载，
/// 顶部只留"怎么筛"。
export function Toolbar({
  filters,
  selection,
  className,
}: {
  filters?: React.ReactNode
  /// 右侧区（计数、发布按钮等）。
  selection?: React.ReactNode
  className?: string
}) {
  return (
    <div
      className={cn(
        'flex flex-wrap items-center justify-between gap-3 rounded-lg border border-border bg-card px-3 py-2.5 shadow-card',
        className,
      )}
    >
      <div className="flex flex-wrap items-center gap-2">{filters}</div>
      {selection !== undefined && (
        <div className="flex flex-wrap items-center gap-2">{selection}</div>
      )}
    </div>
  )
}

/// 页面内容的统一纵向节奏。
export function PageBody({ className, ...props }: React.HTMLAttributes<HTMLDivElement>) {
  return <div className={cn('flex flex-col gap-4 animate-fade-in', className)} {...props} />
}
