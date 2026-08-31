import { X } from 'lucide-react'
import { useEffect } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'

interface DrawerProps {
  open: boolean
  onClose: () => void
  title: string
  /// 一句话说明这个抽屉在做什么。表单字段再清楚，也不如先告诉用户"这一步是干什么的"。
  description?: string
  /// 底部固定操作区（保存/取消）。放在底部而非表单末尾，长表单滚动时按钮始终可达。
  footer?: React.ReactNode
  children: React.ReactNode
}

/// 右侧抽屉：承载单条记录的新建与编辑。
///
/// 为什么不继续用内联表单：列表页原本把"新建表单 + 列表 + 展开式编辑器"堆在一屏，
/// 用户要先滚过一大片表单才看到数据，且编辑时的上下文（改的是哪一条）容易丢。
/// 抽屉把"浏览"与"编辑"分成两个层次：列表始终是列表，编辑浮在其上并明确标题指向对象。
export function Drawer({ open, onClose, title, description, footer, children }: DrawerProps) {
  const { t } = useTranslation()

  // Esc 关闭：抽屉是模态层，键盘用户需要一个不用找关闭按钮的退出方式
  useEffect(() => {
    if (!open) return undefined
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose()
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [open, onClose])

  if (!open) return null

  return (
    <div className="fixed inset-0 z-40 flex justify-end">
      <button
        type="button"
        aria-label={t('common:close')}
        className="absolute inset-0 bg-black/40"
        onClick={onClose}
      />
      <aside
        role="dialog"
        aria-modal="true"
        aria-label={title}
        className="relative z-10 flex h-full w-full max-w-xl flex-col border-l border-border bg-card shadow-xl"
      >
        <header className="flex items-start justify-between gap-3 border-b border-border px-5 py-4">
          <div className="flex flex-col gap-1">
            <h2 className="text-sm font-semibold">{title}</h2>
            {description !== undefined && (
              <p className="text-xs text-muted-foreground">{description}</p>
            )}
          </div>
          <Button size="icon" variant="ghost" aria-label={t('common:close')} onClick={onClose}>
            <X className="h-4 w-4" />
          </Button>
        </header>

        <div className="flex-1 overflow-y-auto px-5 py-4">{children}</div>

        {footer !== undefined && (
          <footer className="flex items-center justify-end gap-2 border-t border-border px-5 py-3">
            {footer}
          </footer>
        )}
      </aside>
    </div>
  )
}

/// 抽屉内的字段分区：把十几个字段按语义切成"基本/调度/计费行为/可见性"这类段落。
/// 一长条无分隔的表单，用户无法判断哪些字段该一起考虑。
export function FieldGroup({
  title,
  hint,
  children,
}: {
  title: string
  hint?: string
  children: React.ReactNode
}) {
  return (
    <section className="flex flex-col gap-3 border-t border-border py-4 first:border-t-0 first:pt-0">
      <div className="flex flex-col gap-0.5">
        <h3 className="text-xs font-semibold tracking-wide text-muted-foreground uppercase">
          {title}
        </h3>
        {hint !== undefined && <p className="text-xs text-muted-foreground/80">{hint}</p>}
      </div>
      {children}
    </section>
  )
}
