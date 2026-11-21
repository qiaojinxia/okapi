import { cn } from '@/lib/utils'

/// 所有文本类控件共用的外观：边框 + 轻阴影，悬停加深边框，聚焦主色环。
export const inputClass =
  'rounded-lg border border-input bg-card text-sm shadow-xs outline-none transition-[border-color,box-shadow] duration-150 placeholder:text-muted-foreground/65 hover:border-muted-foreground/45 focus-visible:border-primary focus-visible:ring-2 focus-visible:ring-primary/25 aria-invalid:border-destructive/75 aria-invalid:focus-visible:border-destructive aria-invalid:focus-visible:ring-destructive/20 disabled:cursor-not-allowed disabled:opacity-60 read-only:bg-muted/40'

export function Input({ className, ...props }: React.InputHTMLAttributes<HTMLInputElement>) {
  return <input className={cn(inputClass, 'h-9 w-full px-3', className)} {...props} />
}

export function Textarea({
  className,
  ...props
}: React.TextareaHTMLAttributes<HTMLTextAreaElement>) {
  return (
    <textarea className={cn(inputClass, 'min-h-24 w-full px-3 py-2 leading-6', className)} {...props} />
  )
}

export function Label({ className, ...props }: React.LabelHTMLAttributes<HTMLLabelElement>) {
  return (
    <label
      className={cn('text-xs font-medium text-muted-foreground select-none', className)}
      {...props}
    />
  )
}
