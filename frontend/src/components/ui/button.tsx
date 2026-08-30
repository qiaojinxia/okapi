import { type VariantProps, cva } from 'class-variance-authority'
import { cn } from '@/lib/utils'

const buttonVariants = cva(
  'inline-flex items-center justify-center gap-2 rounded-md text-sm font-medium transition-colors disabled:pointer-events-none disabled:opacity-50 outline-none focus-visible:ring-2 focus-visible:ring-primary/40',
  {
    variants: {
      variant: {
        default: 'bg-primary text-primary-foreground hover:bg-primary/90',
        outline: 'border border-border bg-card hover:bg-muted',
        ghost: 'hover:bg-muted',
        destructive: 'bg-destructive text-primary-foreground hover:bg-destructive/90',
      },
      size: {
        default: 'h-9 px-4',
        sm: 'h-8 px-3 text-xs',
        icon: 'h-9 w-9',
      },
    },
    defaultVariants: { variant: 'default', size: 'default' },
  },
)

interface ButtonProps
  extends React.ButtonHTMLAttributes<HTMLButtonElement>,
    VariantProps<typeof buttonVariants> {}

export function Button({ className, variant, size, type, ...props }: ButtonProps) {
  return (
    <button
      type={type ?? 'button'}
      className={cn(buttonVariants({ variant, size }), className)}
      {...props}
    />
  )
}
