import type { LucideIcon } from 'lucide-react'
import { cn } from '@/lib/utils'

export interface SegmentedOption<T extends string | number> {
  value: T
  label: string
  icon?: LucideIcon
  disabled?: boolean
}

interface SegmentedProps<T extends string | number> {
  options: SegmentedOption<T>[]
  value: T
  onChange: (value: T) => void
  size?: 'sm' | 'md'
  /// 给屏幕阅读器的分组名。
  ariaLabel?: string
  className?: string
}

/// 分段选择器：一组互斥的短选项（时间窗 7/30/90 天、本密钥/全账户、1K/1M）。
///
/// 替代此前"几个实心按钮并排、选中的那个换成主色"的做法——那种写法让选项看起来
/// 像一排可以各自点的动作按钮，而不是一个单选。用 `<button aria-pressed>` 而非
/// radio：语义足够，且不会与页面上的页签（role=tab）混淆。
export function Segmented<T extends string | number>({
  options,
  value,
  onChange,
  size = 'md',
  ariaLabel,
  className,
}: SegmentedProps<T>) {
  return (
    <div
      role="group"
      aria-label={ariaLabel}
      className={cn(
        'inline-flex max-w-full items-center gap-0.5 overflow-x-auto rounded-lg border border-border bg-muted/60 p-0.5 scrollbar-none',
        className,
      )}
    >
      {options.map((o) => {
        const active = o.value === value
        return (
          <button
            key={String(o.value)}
            type="button"
            aria-pressed={active}
            disabled={o.disabled}
            onClick={() => onChange(o.value)}
            className={cn(
              'inline-flex shrink-0 items-center justify-center gap-1.5 rounded-md font-medium whitespace-nowrap transition-all outline-none',
              'focus-visible:ring-2 focus-visible:ring-primary/40 disabled:pointer-events-none disabled:opacity-50',
              size === 'sm' ? 'h-7 px-2.5 text-xs' : 'h-8 px-3 text-sm',
              active
                ? 'bg-card text-foreground shadow-card'
                : 'text-muted-foreground hover:text-foreground',
            )}
          >
            {o.icon && <o.icon className={size === 'sm' ? 'h-3.5 w-3.5' : 'h-4 w-4'} />}
            {o.label}
          </button>
        )
      })}
    </div>
  )
}
