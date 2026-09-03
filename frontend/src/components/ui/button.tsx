import { type VariantProps, cva } from 'class-variance-authority'
import { Loader2 } from 'lucide-react'
import { cn } from '@/lib/utils'

// whitespace-nowrap：按钮文字永不折行（窄屏下 "Export CSV" 曾被挤成两行）——
// 空间不够该由父级工具栏换行解决，而不是让单个按钮变成两行高的方块
const buttonVariants = cva(
  [
    'inline-flex items-center justify-center gap-2 rounded-md text-sm font-medium whitespace-nowrap',
    'transition-[color,background-color,border-color,box-shadow,transform] duration-150 outline-none select-none',
    'focus-visible:ring-2 focus-visible:ring-primary/40 focus-visible:ring-offset-1 focus-visible:ring-offset-background',
    'disabled:pointer-events-none disabled:opacity-50 active:scale-[0.985]',
    '[&_svg]:shrink-0',
  ].join(' '),
  {
    variants: {
      variant: {
        default: 'bg-primary text-primary-foreground shadow-xs hover:bg-primary/90',
        secondary: 'bg-accent text-foreground hover:bg-accent/70',
        outline: 'border border-border bg-card shadow-xs hover:bg-accent/60 hover:border-muted-foreground/30',
        ghost: 'hover:bg-accent/70 text-foreground',
        link: 'h-auto px-0 text-primary underline-offset-4 hover:underline',
        destructive: 'bg-destructive text-primary-foreground shadow-xs hover:bg-destructive/90',
      },
      size: {
        default: 'h-9 px-4',
        sm: 'h-8 px-3 text-xs',
        xs: 'h-7 px-2.5 text-xs',
        lg: 'h-10 px-5',
        icon: 'h-9 w-9',
      },
    },
    defaultVariants: { variant: 'default', size: 'default' },
  },
)

interface ButtonProps
  extends React.ButtonHTMLAttributes<HTMLButtonElement>,
    VariantProps<typeof buttonVariants> {
  /// 进行中：禁用 + 旋转图标替换掉左侧图标。比把文字换成"加载中…"更稳——
  /// 文字换掉会让按钮宽度跳动，也让用户找不到自己刚点的是哪个。
  loading?: boolean
}

export function Button({
  className,
  variant,
  size,
  type,
  loading = false,
  disabled,
  children,
  ...props
}: ButtonProps) {
  return (
    <button
      type={type ?? 'button'}
      className={cn(buttonVariants({ variant, size }), className)}
      disabled={disabled || loading}
      aria-busy={loading || undefined}
      {...props}
    >
      {loading && <Loader2 className="h-4 w-4 animate-spin" />}
      {children}
    </button>
  )
}

export { buttonVariants }
