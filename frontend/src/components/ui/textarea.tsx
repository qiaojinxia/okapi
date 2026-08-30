import { cn } from '@/lib/utils'

export function Textarea({
  className,
  ...props
}: React.TextareaHTMLAttributes<HTMLTextAreaElement>) {
  return (
    <textarea
      className={cn(
        'w-full rounded-md border border-input bg-card p-3 font-mono text-xs outline-none placeholder:text-muted-foreground focus-visible:ring-2 focus-visible:ring-primary/40',
        className,
      )}
      {...props}
    />
  )
}
