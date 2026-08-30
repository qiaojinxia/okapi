import { type VariantProps, cva } from 'class-variance-authority'
import { cn } from '@/lib/utils'

const badgeVariants = cva(
  'inline-flex items-center rounded-full px-2 py-0.5 text-xs font-medium',
  {
    variants: {
      variant: {
        default: 'bg-primary/10 text-primary',
        success: 'bg-success/15 text-success',
        destructive: 'bg-destructive/15 text-destructive',
        muted: 'bg-muted text-muted-foreground',
      },
    },
    defaultVariants: { variant: 'default' },
  },
)

interface BadgeProps
  extends React.HTMLAttributes<HTMLSpanElement>,
    VariantProps<typeof badgeVariants> {}

export function Badge({ className, variant, ...props }: BadgeProps) {
  return <span className={cn(badgeVariants({ variant }), className)} {...props} />
}
