import type { LucideIcon } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { cn } from '@/lib/utils'

interface IconButtonProps {
  icon: LucideIcon
  /// 同时用作 tooltip 与可访问名字——图标按钮没有可见文字，
  /// 缺了它对屏幕阅读器就是一个无名按钮。
  label: string
  onClick: () => void
  variant?: 'outline' | 'ghost' | 'destructive'
  disabled?: boolean
  className?: string
}

/// 图标动作按钮。
///
/// 表格行动作此前是一排文字按钮（停用/测试/编辑/复制/删除），在被其他列挤窄后
/// 每个按钮各占一行，行高涨到近百像素、一屏只看得到两三条记录。
/// 图标形态让五个动作回到同一行，行高恢复紧凑。
export function IconButton({
  icon: Icon,
  label,
  onClick,
  variant = 'ghost',
  disabled = false,
  className,
}: IconButtonProps) {
  return (
    <Button
      size="icon"
      variant={variant}
      title={label}
      aria-label={label}
      disabled={disabled}
      onClick={onClick}
      className={cn('h-7 w-7', className)}
    >
      <Icon className="h-3.5 w-3.5" />
    </Button>
  )
}
