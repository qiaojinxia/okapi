import { cn } from '@/lib/utils'

interface SwitchProps {
  checked: boolean
  onChange: (checked: boolean) => void
  label: string
  /// 说明这个开关打开后会发生什么。布尔开关最容易踩的坑是名字看得懂、后果看不懂
  /// （"按响应模型计费"打开后账单依据就变了），故把后果写在开关旁边而不是文档里。
  description?: string
  disabled?: boolean
}

/// 布尔开关。替代此前让用户手写 `{"thinking_to_content":true}` 的 JSON 文本框——
/// 那种输入既无法发现有哪些可选项，也无法校验拼写。
export function Switch({ checked, onChange, label, description, disabled = false }: SwitchProps) {
  return (
    <label
      className={cn(
        'flex items-start justify-between gap-4',
        disabled ? 'cursor-not-allowed opacity-50' : 'cursor-pointer',
      )}
    >
      <span className="flex flex-col gap-0.5">
        <span className="text-sm">{label}</span>
        {description !== undefined && (
          <span className="text-xs text-muted-foreground">{description}</span>
        )}
      </span>
      <span className="relative inline-flex shrink-0 pt-0.5">
        <input
          type="checkbox"
          role="switch"
          checked={checked}
          disabled={disabled}
          aria-label={label}
          onChange={(e) => onChange(e.target.checked)}
          className="peer h-5 w-9 cursor-inherit appearance-none rounded-full bg-input outline-none transition-colors checked:bg-primary focus-visible:ring-2 focus-visible:ring-primary/40 disabled:cursor-not-allowed"
        />
        <span className="pointer-events-none absolute top-1 left-1 h-3 w-3 rounded-full bg-card transition-transform peer-checked:translate-x-4" />
      </span>
    </label>
  )
}
