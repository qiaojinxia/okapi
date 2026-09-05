import { Search, X } from 'lucide-react'
import { useRef } from 'react'
import { useTranslation } from 'react-i18next'
import { cn } from '@/lib/utils'

interface SearchInputProps
  extends Omit<React.InputHTMLAttributes<HTMLInputElement>, 'onChange' | 'value'> {
  value: string
  onChange: (value: string) => void
  /// 回车提交（服务端检索的页面用；纯前端过滤的页面不传）。
  onSubmit?: () => void
  className?: string
  inputClassName?: string
}

/// 搜索框：放大镜前缀 + 非空时的清空按钮。
///
/// 列表页的搜索框此前与普通输入框长得一样，还得配一个"搜索"标签才认得出——
/// 放大镜图标本身就是标签，省下的那行让工具栏矮一档。
export function SearchInput({
  value,
  onChange,
  onSubmit,
  onKeyDown,
  className,
  inputClassName,
  ...props
}: SearchInputProps) {
  const { t } = useTranslation()
  const input = useRef<HTMLInputElement>(null)
  return (
    <div className={cn('relative flex items-center', className)}>
      <Search className="pointer-events-none absolute left-2.5 h-4 w-4 text-muted-foreground" />
      <input
        ref={input}
        type="search"
        value={value}
        onChange={(e) => onChange(e.target.value)}
        onKeyDown={(e) => {
          if (e.nativeEvent.isComposing || e.nativeEvent.keyCode === 229) return
          if (e.key === 'Enter' && onSubmit) {
            e.preventDefault()
            onSubmit()
          }
          onKeyDown?.(e)
        }}
        className={cn(
          'h-9 w-full rounded-md border border-input bg-card pr-8 pl-8 text-sm shadow-xs outline-none transition-colors',
          'placeholder:text-muted-foreground hover:border-muted-foreground/40 focus-visible:border-primary focus-visible:ring-2 focus-visible:ring-primary/25',
          '[&::-webkit-search-cancel-button]:appearance-none',
          inputClassName,
        )}
        {...props}
      />
      {value !== '' && (
        <button
          type="button"
          aria-label={t('common:clear')}
          onClick={() => {
            onChange('')
            input.current?.focus({ preventScroll: true })
          }}
          className="absolute right-0 flex h-full w-8 items-center justify-center rounded text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:ring-2 focus-visible:ring-primary/40"
        >
          <X className="h-3.5 w-3.5" />
        </button>
      )}
    </div>
  )
}
