import { type VariantProps, cva } from 'class-variance-authority'
import { cn } from '@/lib/utils'

const badgeVariants = cva(
  'inline-flex max-w-full items-center gap-1 rounded-full px-2 py-0.5 text-xs font-medium whitespace-nowrap',
  {
    variants: {
      variant: {
        default: 'bg-primary/10 text-primary',
        success: 'bg-success/14 text-success',
        warning: 'bg-warning/16 text-warning',
        destructive: 'bg-destructive/12 text-destructive',
        info: 'bg-info/14 text-info',
        muted: 'bg-muted text-muted-foreground',
        outline: 'border border-border bg-transparent text-foreground',
      },
    },
    defaultVariants: { variant: 'default' },
  },
)

interface BadgeProps
  extends React.HTMLAttributes<HTMLSpanElement>,
    VariantProps<typeof badgeVariants> {
  /// 左侧状态点：状态徽章（启用 / 冷却 / 失败）比纯文字多一个可扫视的色块。
  dot?: boolean
}

export function Badge({ className, variant, dot = false, children, ...props }: BadgeProps) {
  return (
    <span className={cn(badgeVariants({ variant }), className)} {...props}>
      {dot && <span className="h-1.5 w-1.5 shrink-0 rounded-full bg-current opacity-80" aria-hidden />}
      {children}
    </span>
  )
}
