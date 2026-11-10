import { X } from 'lucide-react'
import { cn } from '@/lib/utils'

/// 生效中的筛选条件：「维度 值 ×」的小标签。
///
/// 收在浮层 / 抽屉里的筛选一旦生效，就必须在页面上留个看得见的痕迹——否则列表为什么变少了无从得知。
/// 点 × 立即移除这一条；标签只占一行的高度，不会把表格往下推。
export function FilterChip({ label, value, removeLabel, onRemove, className }: {
  /// 维度名（用户 / 渠道 / 错误码）。
  label: string
  value: string
  /// × 按钮的可访问名称（「移除过滤 alice」）。
  removeLabel: string
  onRemove: () => void
  className?: string
}) {
  return (
    <span data-slot="filter-chip" className={cn('inline-flex h-7 max-w-full items-center gap-1 rounded-full bg-primary/10 pr-0.5 pl-2.5 text-xs text-primary', className)}>
      <span className="shrink-0 text-primary/70">{label}</span>
      <span className="min-w-0 max-w-64 truncate font-medium" title={value}>{value}</span>
      <button
        type="button"
        aria-label={removeLabel}
        className="flex h-6 w-6 shrink-0 items-center justify-center rounded-full outline-none hover:bg-primary/15 focus-visible:ring-2 focus-visible:ring-primary/40"
        onClick={onRemove}
      >
        <X aria-hidden className="h-3 w-3" />
      </button>
    </span>
  )
}

/// 一排筛选标签：窄屏换行，多于一条时由调用方在末尾放「清除全部」。
export function FilterChips({ label, className, children }: { label: string; className?: string; children: React.ReactNode }) {
  return (
    <div role="group" aria-label={label} data-slot="filter-chips" className={cn('flex min-w-0 flex-wrap items-center gap-1.5', className)}>
      {children}
    </div>
  )
}
