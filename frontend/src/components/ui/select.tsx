import { ChevronDown } from 'lucide-react'
import { inputClass } from '@/components/ui/input'
import { cn } from '@/lib/utils'

export interface SelectOption {
  value: string
  label: string
  /// 仍列出但不可选（label 里自带原因，比悄悄隐藏更能让用户明白为什么）。
  disabled?: boolean
}

interface SelectProps {
  value: string
  onChange: (value: string) => void
  options: SelectOption[]
  /// 置顶的空值项（如"全部"/"不改"），省得每个调用方自己拼一个 option。
  placeholder?: string
  /// `md` 为标准表单高度（36px），`sm` 为图表/卡片工具栏紧凑高度（32px）。
  size?: 'sm' | 'md'
  id?: string
  disabled?: boolean
  className?: string
  'aria-label'?: string
}

/// 原生 `select` 外观统一封装。
///
/// 不做自绘下拉：原生 select 在移动端会调起系统选择器（比自绘浮层好用得多），
/// 且键盘与屏幕阅读器行为免费正确。样式统一即可，交互不必重造。
export function Select({
  value,
  onChange,
  options,
  placeholder,
  size = 'md',
  id,
  disabled = false,
  className,
  ...rest
}: SelectProps) {
  return (
    <span className={cn('relative inline-flex', className)}>
      <select
        id={id}
        value={value}
        disabled={disabled}
        aria-label={rest['aria-label']}
        onChange={(e) => onChange(e.target.value)}
        className={cn(
          inputClass,
          'w-full appearance-none',
          size === 'sm' ? 'h-8 py-0 pr-7 pl-2.5 text-xs' : 'h-9 pr-8 pl-3',
        )}
      >
        {placeholder !== undefined && <option value="">{placeholder}</option>}
        {options.map((o) => (
          <option key={o.value} value={o.value} disabled={o.disabled}>
            {o.label}
          </option>
        ))}
      </select>
      <ChevronDown
        className={cn(
          'pointer-events-none absolute top-1/2 -translate-y-1/2 text-muted-foreground/75',
          size === 'sm' ? 'right-2 h-3.5 w-3.5' : 'right-2.5 h-3.5 w-3.5',
        )}
      />
    </span>
  )
}
