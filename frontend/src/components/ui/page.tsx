import { cn } from '@/lib/utils'

/// 页头：标题 + 这一页负责什么 + 主操作（通常是"新建"）。
///
/// 一页一职责的前提是用户能一眼知道这页管什么。此前列表页直接甩出表格，
/// 页面之间只靠侧栏高亮区分，同屏还混着别的资源的表单。
///
/// 只有文字：没有图标方块，计数 / 状态写成标题后面的一句朴素文字（「共 5 条 · 4 启用」），
/// 不再套徽章胶囊；主操作在桌面端与「标题 + 说明」这一块垂直居中。
export function PageHeader({
  title,
  description,
  meta,
  action,
  className,
  compact = false,
}: {
  title: string
  description?: string
  /// 标题后面的一句统计 / 状态（朴素文字）。
  meta?: React.ReactNode
  action?: React.ReactNode
  className?: string
  /// 数据密集页在桌面端将标题与说明并排，给表格保留更多高度。
  compact?: boolean
}) {
  return (
    <header data-slot="page-header" className={cn('flex min-w-0 shrink-0 flex-col gap-4 lg:flex-row lg:items-center lg:justify-between', className)}>
      <div className={cn('flex min-w-0 flex-col gap-1', compact && 'lg:flex-row lg:items-center lg:gap-3')}>
        <div className="flex flex-wrap items-baseline gap-x-3 gap-y-1">
          <h1 className="text-xl font-semibold leading-7 tracking-tight">{title}</h1>
          {meta !== undefined && meta !== null && meta !== false && (
            <span className="text-sm text-muted-foreground tabular-nums">{meta}</span>
          )}
        </div>
        {description !== undefined && (
          <p className={cn('max-w-3xl text-sm leading-6 text-muted-foreground', compact && 'text-xs leading-5')}>{description}</p>
        )}
      </div>
      {action !== undefined && <div className="flex shrink-0 flex-wrap items-center gap-2 [&_button]:min-h-10 lg:justify-end lg:[&_button]:min-h-9">{action}</div>}
    </header>
  )
}

/// 工具栏：搜索/过滤在左，右侧放计数或次级动作。
///
/// 不再包一层卡片：筛选控件直接放在页面底色上，表格才是页面里唯一的「纸」。
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
      className={cn('flex min-w-0 shrink-0 flex-wrap items-center justify-between gap-x-4 gap-y-3', className)}
    >
      <div className={cn('flex min-w-0 max-w-full flex-1 basis-96 flex-wrap items-center gap-3', filtersClassName)}>{filters}</div>
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
