import { type VariantProps, cva } from 'class-variance-authority'
import { cn } from '@/lib/utils'

const badgeVariants = cva(
  'inline-flex min-h-6 max-w-full items-center gap-1 rounded-full border border-transparent px-2 py-0.5 text-xs leading-4 font-medium whitespace-nowrap',
  {
    variants: {
      variant: {
        default: 'bg-primary/10 text-primary ring-1 ring-inset ring-primary/15',
        success: 'bg-success/12 text-success ring-1 ring-inset ring-success/20',
        warning: 'bg-warning/14 text-warning ring-1 ring-inset ring-warning/25',
        destructive: 'bg-destructive/10 text-destructive ring-1 ring-inset ring-destructive/20',
        info: 'bg-info/12 text-info ring-1 ring-inset ring-info/20',
        muted: 'bg-muted/85 text-muted-foreground ring-1 ring-inset ring-border/55',
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
