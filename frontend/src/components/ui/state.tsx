import { AlertCircle, Inbox, Loader2, RotateCw } from 'lucide-react'
import type { LucideIcon } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { cn } from '@/lib/utils'

/// 空态：比一行"暂无数据"多给一句下一步动作——管理面的空态通常意味着
/// "还没配"，直接告诉用户去哪配比让他自己找强。可带一个主动作按钮（如"新建渠道"）。
export function EmptyState({
  icon: Icon = Inbox,
  title,
  hint,
  action,
  className,
}: {
  icon?: LucideIcon
  title?: string
  hint?: string
  action?: React.ReactNode
  className?: string
}) {
  const { t } = useTranslation()
  return (
    <div
      className={cn(
        'flex flex-col items-center justify-center gap-2 rounded-lg border border-dashed border-border bg-card/50 px-6 py-12 text-center animate-fade-in',
        className,
      )}
    >
      <span className="flex h-11 w-11 items-center justify-center rounded-full bg-muted text-muted-foreground">
        <Icon className="h-5 w-5" />
      </span>
      <span className="mt-1 text-sm font-medium">{title ?? t('common:empty')}</span>
      {hint !== undefined && (
        <span className="max-w-md text-xs leading-5 text-muted-foreground">{hint}</span>
      )}
      {action !== undefined && <div className="mt-2">{action}</div>}
    </div>
  )
}

/// 加载态：替代散落各处的"加载中…"纯文本。
export function LoadingState({ className }: { className?: string }) {
  const { t } = useTranslation()
  return (
    <div
      role="status"
      className={cn('flex items-center justify-center gap-2 py-10 text-muted-foreground', className)}
    >
      <Loader2 className="h-4 w-4 animate-spin" />
      <span className="text-sm">{t('common:loading')}</span>
    </div>
  )
}

/// 错误态：带图标的红色提示块，可挂"重试"。
export function ErrorState({
  message,
  onRetry,
  className,
}: {
  message: string
  onRetry?: () => void
  className?: string
}) {
  const { t } = useTranslation()
  return (
    <div
      role="alert"
      className={cn(
        'flex items-start gap-3 rounded-lg border border-destructive/30 bg-destructive/8 px-4 py-3 text-sm',
        className,
      )}
    >
      <AlertCircle className="mt-0.5 h-4 w-4 shrink-0 text-destructive" />
      <span className="flex-1 text-destructive">{message}</span>
      {onRetry && (
        <Button size="xs" variant="outline" onClick={onRetry}>
          <RotateCw className="h-3 w-3" />
          {t('common:retry')}
        </Button>
      )}
    </div>
  )
}
