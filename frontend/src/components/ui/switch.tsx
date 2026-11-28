import { cn } from '@/lib/utils'

interface SwitchProps {
  checked: boolean
  onChange: (checked: boolean) => void
  label: string
  /// 说明这个开关打开后会发生什么。布尔开关最容易踩的坑是名字看得懂、后果看不懂
  /// （"按响应模型计费"打开后账单依据就变了），故把后果写在开关旁边而不是文档里。
  description?: string
  disabled?: boolean
  /// 只留给读屏的标签：表格行内的开关由列头说明，可见文字只会挤占列宽。
  hideLabel?: boolean
  /// `card`：独立卡片化开关行（白底细边微阴影），统一表单与设置页中的开关条目外观。
  variant?: 'plain' | 'card'
  className?: string
}

/// 布尔开关。替代此前让用户手写 `{"thinking_to_content":true}` 的 JSON 文本框——
/// 那种输入既无法发现有哪些可选项，也无法校验拼写。
export function Switch({
  checked,
  onChange,
  label,
  description,
  disabled = false,
  hideLabel = false,
  variant = 'plain',
  className,
}: SwitchProps) {
  return (
    <label
      className={cn(
        'flex items-start justify-between gap-4',
        variant === 'card' &&
          'rounded-lg border border-border/75 bg-card px-3.5 py-3 shadow-xs transition-colors hover:border-muted-foreground/30',
        disabled ? 'cursor-not-allowed opacity-50' : 'cursor-pointer',
        className,
      )}
    >
      <span className={cn('flex min-w-0 flex-col gap-0.5', hideLabel && 'sr-only')}>
        <span className="text-sm">{label}</span>
        {description !== undefined && (
          <span className="text-xs leading-5 text-muted-foreground">{description}</span>
        )}
      </span>
      {/*
        轨道用 span 画、input 透明覆盖——不能把 checkbox 本身做成胶囊。
        原生 checkbox 保持 1:1，w-9 会把高度一起撑到 36px，看起来像竖着的药丸。
      */}
      <span className="relative mt-0.5 inline-flex h-5 w-9 shrink-0">
        <input
          type="checkbox"
          role="switch"
          checked={checked}
          disabled={disabled}
          aria-label={label}
          onChange={(e) => onChange(e.target.checked)}
          className="peer absolute inset-0 z-10 cursor-inherit appearance-none opacity-0"
        />
        <span className="h-5 w-9 rounded-full bg-input shadow-[inset_0_1px_2px_oklch(0_0_0/0.12)] transition-colors peer-checked:bg-primary peer-checked:shadow-[inset_0_1px_2px_oklch(0_0_0/0.18)] peer-focus-visible:ring-2 peer-focus-visible:ring-primary/40 peer-focus-visible:ring-offset-1 peer-focus-visible:ring-offset-background" />
        <span className="pointer-events-none absolute top-0.5 left-0.5 h-4 w-4 rounded-full bg-card shadow-[0_1px_3px_oklch(0_0_0/0.22)] transition-transform peer-checked:translate-x-4" />
      </span>
    </label>
  )
}
