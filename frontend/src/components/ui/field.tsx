import { Label } from '@/components/ui/input'
import { cn } from '@/lib/utils'

interface FieldProps {
  label: string
  htmlFor?: string
  /// 一句话说明填什么 / 有什么后果。
  hint?: string
  /// 字段级错误（红字，替换 hint）。
  error?: string | null
  required?: boolean
  className?: string
  children: React.ReactNode
}

/// 表单字段：标签 + 控件 + 说明/错误。
///
/// 把项目里重复了几十次的 `<div className="flex flex-col gap-1.5"><Label/>…</div>`
/// 收成一个组件，顺带把"错误说明放哪"这件事定下来——就放在控件正下方，
/// 而不是表单底部一句总的"参数有误"。
export function Field({ label, htmlFor, hint, error, required, className, children }: FieldProps) {
  return (
    <div className={cn('flex min-w-0 flex-col gap-1.5', className)}>
      <Label htmlFor={htmlFor}>
        {label}
        {required && <span className="ml-0.5 text-destructive">*</span>}
      </Label>
      {children}
      {error ? (
        <span className="text-xs text-destructive">{error}</span>
      ) : hint !== undefined ? (
        <span className="text-xs text-muted-foreground/90">{hint}</span>
      ) : null}
    </div>
  )
}
