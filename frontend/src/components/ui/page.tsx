import { cn } from '@/lib/utils'

/// 页头：标题 + 这一页负责什么 + 主操作（通常是"新建"）。
///
/// 一页一职责的前提是用户能一眼知道这页管什么。此前列表页直接甩出表格，
/// 页面之间只靠侧栏高亮区分，同屏还混着别的资源的表单。
export function PageHeader({
  title,
  description,
  action,
}: {
  title: string
  description?: string
  action?: React.ReactNode
}) {
  return (
    <header className="flex flex-wrap items-start justify-between gap-3">
      <div className="flex flex-col gap-1">
        <h1 className="text-lg font-semibold">{title}</h1>
        {description !== undefined && (
          <p className="text-xs text-muted-foreground">{description}</p>
        )}
      </div>
      {action}
    </header>
  )
}

/// 工具栏：搜索/过滤在左，批量操作在右。
///
/// 批量操作只在选中若干条后出现（`selection` 非空），避免无对象时摆一排死按钮
/// 让人猜"这是对全部生效还是对选中生效"。
export function Toolbar({
  filters,
  selection,
  className,
}: {
  filters?: React.ReactNode
  selection?: React.ReactNode
  className?: string
}) {
  return (
    <div
      className={cn(
        'flex flex-wrap items-end justify-between gap-3 rounded-lg border border-border bg-muted/30 px-3 py-2.5',
        className,
      )}
    >
      <div className="flex flex-wrap items-end gap-2">{filters}</div>
      {selection !== undefined && (
        <div className="flex flex-wrap items-center gap-2">{selection}</div>
      )}
    </div>
  )
}

/// 页内分段：同一职责下的次级视图（如"模型定价"页里的 倍率 / 固定单价 两种模式）。
export function Tabs({
  tabs,
  active,
  onChange,
}: {
  tabs: { id: string; label: string }[]
  active: string
  onChange: (id: string) => void
}) {
  return (
    <div role="tablist" className="flex gap-1 border-b border-border">
      {tabs.map((tb) => (
        <button
          key={tb.id}
          type="button"
          role="tab"
          aria-selected={active === tb.id}
          onClick={() => onChange(tb.id)}
          className={cn(
            '-mb-px border-b-2 px-3 py-2 text-sm transition-colors',
            active === tb.id
              ? 'border-primary font-medium text-foreground'
              : 'border-transparent text-muted-foreground hover:text-foreground',
          )}
        >
          {tb.label}
        </button>
      ))}
    </div>
  )
}
