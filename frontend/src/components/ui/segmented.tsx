import { useEffect, useId, useRef, useState } from 'react'
import type { LucideIcon } from 'lucide-react'
import { cn } from '@/lib/utils'
import { ChoiceRail, revealChoice } from './choice-rail'

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
  const container = useRef<HTMLDivElement>(null)
  const id = useId()
  const [focused, setFocused] = useState(value)
  const enabled = options.filter((option) => !option.disabled)
  const contentKey = JSON.stringify(options.map((option) => [option.value, option.label, !!option.icon]))
  const entry = enabled.some((option) => option.value === focused) ? focused : enabled.find((option) => option.value === value)?.value ?? enabled[0]?.value
  useEffect(() => { setFocused(value) }, [value])
  useEffect(() => {
    const node = container.current
    if (!node) return
    const reveal = () => {
      const selected = node.contains(document.activeElement) ? document.activeElement as HTMLElement : node.querySelector<HTMLElement>('[aria-pressed="true"]')
      if (!selected) return
      revealChoice(node, selected)
    }
    reveal()
    const observer = new ResizeObserver(reveal)
    observer.observe(node)
    for (const option of node.children) observer.observe(option)
    return () => observer.disconnect()
  }, [value, contentKey])
  return (
    <ChoiceRail container={container} label={ariaLabel}>
    <div ref={container}
      id={id}
      role="group"
      aria-label={ariaLabel}
      onBlur={(e) => { if (!e.currentTarget.contains(e.relatedTarget)) setFocused(value) }}
      className={cn(
        'inline-flex min-w-0 max-w-full items-center gap-0.5 overflow-x-auto rounded-lg border border-border bg-muted/60 p-0.5 scrollbar-none',
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
            tabIndex={entry === o.value ? 0 : -1}
            onFocus={(e) => { setFocused(o.value); if (container.current) revealChoice(container.current, e.currentTarget) }}
            onKeyDown={(e) => {
              if (e.altKey || e.ctrlKey || e.metaKey) return
              const index = enabled.findIndex((item) => item.value === o.value)
              const next = e.key === 'Home' ? 0 : e.key === 'End' ? enabled.length - 1
                : e.key === 'ArrowRight' ? (index + 1) % enabled.length
                  : e.key === 'ArrowLeft' ? (index - 1 + enabled.length) % enabled.length : null
              if (next === null) return
              e.preventDefault()
              container.current?.querySelectorAll<HTMLButtonElement>('button:not(:disabled)')[next]?.focus({ preventScroll: true })
            }}
            onClick={() => { if (!active) onChange(o.value) }}
            className={cn(
              'inline-flex shrink-0 items-center justify-center gap-1.5 rounded-md font-medium whitespace-nowrap transition-all outline-none',
              'focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-primary/60 disabled:pointer-events-none disabled:opacity-50',
              size === 'sm' ? 'h-11 px-2.5 text-xs md:h-7' : 'h-11 px-3 text-sm md:h-8',
              active
                ? 'bg-card text-foreground shadow-card'
                : 'text-muted-foreground hover:text-foreground',
            )}
          >
            {o.icon && <o.icon aria-hidden className={size === 'sm' ? 'h-3.5 w-3.5' : 'h-4 w-4'} />}
            {o.label}
          </button>
        )
      })}
    </div>
    </ChoiceRail>
  )
}
