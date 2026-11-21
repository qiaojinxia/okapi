import { Check } from 'lucide-react'
import { cn } from '@/lib/utils'

export interface StepItem {
  id: string
  label: string
}

/// 分步引导的步骤条：一件事要按顺序做完几步（新建渠道：接入 → 模型 → 登录授权）时用。
///
/// 与页签的区别：页签是并列的几块内容，随便切；步骤有先后，后面的步骤要等前面的填完才能进，
/// 所以每一步是「已完成 / 当前 / 还没到」三态，还没到的不可点。
/// 已完成的步骤可以点回去改——回头改不丢后面已经填的内容（内容由调用方保持挂载）。
export function Steps({
  items,
  active,
  reachable,
  onSelect,
  ariaLabel,
  className,
}: {
  items: StepItem[]
  active: string
  /// 能直接跳到的最远一步的下标（它之前的步骤都已填完）。
  reachable: number
  onSelect: (id: string) => void
  ariaLabel: string
  className?: string
}) {
  const current = items.findIndex((item) => item.id === active)
  return (
    <nav aria-label={ariaLabel} data-slot="steps" className={cn('min-w-0', className)}>
      <ol className="flex min-w-0 items-center gap-2">
        {items.map((item, index) => {
          const done = index < current
          const isCurrent = index === current
          return (
            <li key={item.id} className={cn('flex min-w-0 items-center gap-2', index > 0 && 'flex-1')}>
              {index > 0 && (
                <span aria-hidden className={cn('h-px min-w-2 flex-1 rounded-full', index <= current ? 'bg-primary/50' : 'bg-border')} />
              )}
              <button
                type="button"
                aria-current={isCurrent ? 'step' : undefined}
                disabled={index > reachable}
                onClick={() => onSelect(item.id)}
                className={cn(
                  // 可收缩：窄屏 / 长文案（英文）下标签截断，而不是把步骤条撑出抽屉
                  'group/step flex min-h-9 min-w-0 items-center gap-2 rounded-lg py-1 pr-2 pl-1 text-sm outline-none transition-colors',
                  'focus-visible:ring-2 focus-visible:ring-primary/40 disabled:cursor-not-allowed',
                  isCurrent ? 'font-medium text-foreground' : 'text-muted-foreground enabled:hover:text-foreground',
                )}
              >
                <span
                  className={cn(
                    'flex h-6 w-6 shrink-0 items-center justify-center rounded-full text-xs font-semibold tabular-nums transition-[color,background-color,box-shadow]',
                    isCurrent
                      ? 'bg-primary text-primary-foreground shadow-[0_2px_8px_-2px_var(--primary)]'
                      : done
                        ? 'bg-primary/12 text-primary ring-1 ring-inset ring-primary/20'
                        : 'border border-border bg-card text-muted-foreground',
                  )}
                >
                  {done ? <Check aria-hidden className="h-3.5 w-3.5" /> : index + 1}
                </span>
                <span className="truncate">{item.label}</span>
              </button>
            </li>
          )
        })}
      </ol>
    </nav>
  )
}
