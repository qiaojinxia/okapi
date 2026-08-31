import { Inbox, Loader2 } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { cn } from '@/lib/utils'

/// 空态：比一行"暂无数据"多给一句下一步动作——管理面的空态通常意味着
/// "还没配"，直接告诉用户去哪配比让他自己找强。
export function EmptyState({ hint, className }: { hint?: string; className?: string }) {
  const { t } = useTranslation()
  return (
    <div
      className={cn(
        'flex flex-col items-center justify-center gap-2 py-10 text-center',
        className,
      )}
    >
      <Inbox className="h-8 w-8 text-muted-foreground/60" />
      <span className="text-sm text-muted-foreground">{t('common:empty')}</span>
      {hint !== undefined && <span className="text-xs text-muted-foreground/80">{hint}</span>}
    </div>
  )
}

/// 加载态：替代散落各处的"加载中…"纯文本。
export function LoadingState({ className }: { className?: string }) {
  const { t } = useTranslation()
  return (
    <div className={cn('flex items-center justify-center gap-2 py-8', className)}>
      <Loader2 className="h-4 w-4 animate-spin text-muted-foreground" />
      <span className="text-sm text-muted-foreground">{t('common:loading')}</span>
    </div>
  )
}

/// 错误态：统一红字块，避免每个页面自己拼 `<p className="text-destructive">`。
export function ErrorState({ message, className }: { message: string; className?: string }) {
  return (
    <p className={cn('rounded-md bg-destructive/10 p-3 text-sm text-destructive', className)}>
      {message}
    </p>
  )
}
