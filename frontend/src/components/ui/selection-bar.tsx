import { X } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { cn } from '@/lib/utils'

interface SelectionBarProps {
  count: number
  onClear: () => void
  children: React.ReactNode
  className?: string
}

/// 批量操作条：选中若干行后从底部浮起，承载"对这 N 条做什么"。
///
/// 此前批量按钮塞在顶部工具栏右侧：勾选发生在表格中下部，动作却在屏幕顶上，
/// 勾到第 20 行时按钮早滚出视野。浮条固定在视口底部，勾到哪都够得着；
/// 无选中时整条不出现，也就不存在"这是对全部还是对选中生效"的歧义。
export function SelectionBar({ count, onClear, children, className }: SelectionBarProps) {
  const { t } = useTranslation()
  if (count <= 0) return null
  return (
    <div className="pointer-events-none fixed inset-x-0 bottom-6 z-40 flex justify-center px-4">
      <div
        role="toolbar"
        aria-label={t('common:selectedCount', { n: count })}
        className={cn(
          'pointer-events-auto flex max-w-full flex-wrap items-center gap-2 rounded-xl border border-border bg-popover/95 px-3 py-2 shadow-popover backdrop-blur animate-fade-up',
          className,
        )}
      >
        <span className="rounded-md bg-primary/10 px-2 py-1 text-xs font-semibold text-primary tabular-nums">
          {t('common:selectedCount', { n: count })}
        </span>
        <span className="mx-1 hidden h-5 w-px bg-border sm:block" aria-hidden />
        {children}
        <Button size="icon" variant="ghost" className="h-8 w-8" aria-label={t('common:clearSelection')} onClick={onClear}>
          <X className="h-4 w-4" />
        </Button>
      </div>
    </div>
  )
}
