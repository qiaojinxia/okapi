import { Check, Minus } from 'lucide-react'
import { cn } from '@/lib/utils'

interface CheckboxProps {
  checked: boolean
  onChange: (checked: boolean) => void
  /// 半选：表头全选框在"部分选中"时用，避免用户误判当前是全选还是全不选。
  indeterminate?: boolean
  disabled?: boolean
  /// 可见文字标签。
  label?: string
  /// 只给屏幕阅读器与测试用的名字，不占版面。
  /// 表格里每行一个勾选框，可见文字会把整列撑宽（且"选中 xxx"这种文案对视力用户是噪音），
  /// 但没有可访问名字又无法定位——故与 `label` 分开。
  srLabel?: string
  className?: string
}

/// 受控多选框。用真实 `input` 承载可访问性与键盘操作（Space 切换、Tab 聚焦），
/// 视觉层用同尺寸的方块覆盖——纯 div 方案要自行补 role/aria/键盘，得不偿失。
export function Checkbox({
  checked,
  onChange,
  indeterminate = false,
  disabled = false,
  label,
  srLabel,
  className,
}: CheckboxProps) {
  return (
    <label
      className={cn(
        'inline-flex items-center gap-2',
        disabled ? 'cursor-not-allowed opacity-50' : 'cursor-pointer',
        className,
      )}
    >
      <span className="relative inline-flex h-4 w-4 shrink-0">
        <input
          type="checkbox"
          checked={checked}
          disabled={disabled}
          aria-label={srLabel ?? label}
          aria-checked={indeterminate ? 'mixed' : checked}
          onChange={(e) => onChange(e.target.checked)}
          className="peer h-4 w-4 cursor-inherit appearance-none rounded border border-input bg-card outline-none checked:border-primary checked:bg-primary focus-visible:ring-2 focus-visible:ring-primary/40 disabled:cursor-not-allowed"
        />
        {(checked || indeterminate) && (
          <span className="pointer-events-none absolute inset-0 flex items-center justify-center text-primary-foreground">
            {indeterminate ? <Minus className="h-3 w-3" /> : <Check className="h-3 w-3" />}
          </span>
        )}
      </span>
      {label !== undefined && <span className="text-sm">{label}</span>}
    </label>
  )
}
