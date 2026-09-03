import { ChevronDown } from 'lucide-react'
import { inputClass } from '@/components/ui/input'
import { cn } from '@/lib/utils'

export interface SelectOption {
  value: string
  label: string
}

interface SelectProps {
  value: string
  onChange: (value: string) => void
  options: SelectOption[]
  /// 置顶的空值项（如"全部"/"不改"），省得每个调用方自己拼一个 option。
  placeholder?: string
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
        className={cn(inputClass, 'h-9 w-full appearance-none pr-8 pl-3')}
      >
        {placeholder !== undefined && <option value="">{placeholder}</option>}
        {options.map((o) => (
          <option key={o.value} value={o.value}>
            {o.label}
          </option>
        ))}
      </select>
      <ChevronDown className="pointer-events-none absolute top-1/2 right-2.5 h-4 w-4 -translate-y-1/2 text-muted-foreground" />
    </span>
  )
}
