import { AlertTriangle, CheckCircle2, Info, X, XCircle } from 'lucide-react'
import type { LucideIcon } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { cn } from '@/lib/utils'

type Tone = 'info' | 'success' | 'warning' | 'destructive'

interface AlertProps {
  tone?: Tone
  title?: string
  icon?: LucideIcon
  children?: React.ReactNode
  /// 右侧动作（如"去登录"）。
  action?: React.ReactNode
  onClose?: () => void
  className?: string
}

const TONE: Record<Tone, { icon: LucideIcon; wrap: string; icon_cls: string }> = {
  info: { icon: Info, wrap: 'border-primary/25 bg-primary/5', icon_cls: 'text-primary' },
  success: { icon: CheckCircle2, wrap: 'border-success/30 bg-success/8', icon_cls: 'text-success' },
  warning: { icon: AlertTriangle, wrap: 'border-warning/35 bg-warning/10', icon_cls: 'text-warning' },
  destructive: {
    icon: XCircle,
    wrap: 'border-destructive/30 bg-destructive/8',
    icon_cls: 'text-destructive',
  },
}

/// 页内提示块：会话降级说明、操作结果回执、表单级错误。
///
/// 替代此前散落的 `<p className="text-xs text-destructive">` / 灰字——一句红色小字
/// 在满屏表格里根本看不见，且"为什么这页什么都没有"这种需要解释的状态值得一个有
/// 图标、有标题、能带按钮的块。
export function Alert({
  tone = 'info',
  title,
  icon,
  children,
  action,
  onClose,
  className,
}: AlertProps) {
  const { t } = useTranslation()
  const Icon = icon ?? TONE[tone].icon
  return (
    <div
      role={tone === 'destructive' ? 'alert' : 'status'}
      className={cn(
        'flex items-start gap-3 rounded-lg border px-4 py-3 text-sm animate-fade-in',
        TONE[tone].wrap,
        className,
      )}
    >
      <Icon className={cn('mt-0.5 h-4 w-4 shrink-0', TONE[tone].icon_cls)} />
      <div className="flex min-w-0 flex-1 flex-col gap-0.5">
        {title !== undefined && <span className="font-medium">{title}</span>}
        {children !== undefined && (
          <div className={cn('text-muted-foreground', title === undefined && 'text-foreground')}>
            {children}
          </div>
        )}
      </div>
      {action}
      {onClose && (
        <button
          type="button"
          aria-label={t('common:close')}
          className="rounded-md p-1 text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
          onClick={onClose}
        >
          <X className="h-4 w-4" />
        </button>
      )}
    </div>
  )
}
