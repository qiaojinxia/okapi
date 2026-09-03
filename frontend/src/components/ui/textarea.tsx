import { inputClass } from '@/components/ui/input'
import { cn } from '@/lib/utils'

/// 等宽文本域：JSON / 批量粘贴用。与 `input.tsx` 的 Textarea 只差字体与内边距。
export function Textarea({
  className,
  ...props
}: React.TextareaHTMLAttributes<HTMLTextAreaElement>) {
  return (
    <textarea
      className={cn(inputClass, 'w-full p-3 font-mono text-xs leading-5', className)}
      {...props}
    />
  )
}
