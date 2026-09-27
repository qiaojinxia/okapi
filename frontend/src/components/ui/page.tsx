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
    <header data-slot="page-header" className={cn('flex min-w-0 shrink-0 flex-col gap-4 lg:flex-row lg:items-start lg:justify-between', className)}>
      <div className="flex min-w-0 items-start gap-3">
        {Icon && (
          <span className="mt-0.5 hidden h-9 w-9 shrink-0 items-center justify-center rounded-lg bg-primary/10 text-primary sm:flex">
            <Icon className="h-4.5 w-4.5" />
          </span>
        )}
        <div className="flex min-w-0 flex-col gap-1">
          <div className="flex flex-wrap items-center gap-2">
            <h1 className="text-xl font-semibold leading-7 tracking-tight">{title}</h1>
            {meta}
          </div>
          {description !== undefined && (
            <p className="max-w-3xl text-sm leading-6 text-muted-foreground">{description}</p>
          )}
        </div>
      </div>
      {action !== undefined && <div className="flex shrink-0 flex-wrap items-center gap-2 [&_button]:min-h-10 lg:justify-end lg:[&_button]:min-h-9">{action}</div>}
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
  filtersClassName,
  selectionClassName,
}: {
  filters?: React.ReactNode
  /// 右侧区（计数、发布按钮等）。
  selection?: React.ReactNode
  className?: string
  filtersClassName?: string
  selectionClassName?: string
}) {
  return (
    <div
      data-slot="toolbar"
      className={cn(
        'flex min-h-15 min-w-0 shrink-0 flex-wrap items-center justify-between gap-x-4 gap-y-3 rounded-xl border border-border bg-card p-3 shadow-card',
        className,
      )}
    >
      <div className={cn('flex min-w-0 max-w-full flex-1 basis-96 flex-wrap items-center gap-3 [&>*]:max-w-full', filtersClassName)}>{filters}</div>
      {selection !== undefined && (
        <div className={cn('flex min-w-0 max-w-full flex-wrap items-center gap-2', selectionClassName)}>{selection}</div>
      )}
    </div>
  )
}

/// 搜索框与提交按钮保持一组；宽屏不无限拉长，窄屏按整组换行。
export function ToolbarSearch({ className, ...props }: React.HTMLAttributes<HTMLDivElement>) {
  return <div data-slot="toolbar-search" className={cn('flex min-w-0 max-w-md flex-1 basis-72 items-center gap-2 [&>button]:shrink-0', className)} {...props} />
}

/// 页面内容的统一纵向节奏。
export function PageBody({ className, ...props }: React.HTMLAttributes<HTMLDivElement>) {
  return <div className={cn('flex flex-col gap-4 animate-fade-in', className)} {...props} />
}
