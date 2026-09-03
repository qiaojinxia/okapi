import type { LucideIcon } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Tooltip } from '@/components/ui/tooltip'
import { cn } from '@/lib/utils'

interface IconButtonProps {
  icon: LucideIcon
  /// 同时用作 tooltip 与可访问名字——图标按钮没有可见文字，
  /// 缺了它对屏幕阅读器就是一个无名按钮。
  label: string
  onClick: () => void
  variant?: 'outline' | 'ghost' | 'destructive'
  disabled?: boolean
  loading?: boolean
  className?: string
}

/// 图标动作按钮。
///
/// 表格行动作此前是一排文字按钮（停用/测试/编辑/复制/删除），在被其他列挤窄后
/// 每个按钮各占一行，行高涨到近百像素、一屏只看得到两三条记录。
/// 图标形态让五个动作回到同一行，行高恢复紧凑。
///
/// 危险动作平时与其他图标同色（灰），悬停才转红：一列里常驻五个红色垃圾桶会把
/// 视线全吸过去，也让"红 = 危险"失去警示力。
export function IconButton({
  icon: Icon,
  label,
  onClick,
  variant = 'ghost',
  disabled = false,
  loading = false,
  className,
}: IconButtonProps) {
  return (
    <Tooltip content={label}>
      <Button
        size="icon"
        variant={variant === 'destructive' ? 'ghost' : variant}
        aria-label={label}
        disabled={disabled}
        loading={loading}
        onClick={(e) => {
          e.stopPropagation()
          onClick()
        }}
        className={cn(
          'h-7 w-7 text-muted-foreground hover:text-foreground [&_svg]:h-3.5 [&_svg]:w-3.5',
          variant === 'destructive' && 'hover:bg-destructive/10 hover:text-destructive',
          className,
        )}
      >
        {!loading && <Icon />}
      </Button>
    </Tooltip>
  )
}
