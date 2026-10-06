import { X } from 'lucide-react'
import { useRef } from 'react'
import { createPortal } from 'react-dom'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { useModalFocus } from '@/hooks/use-modal-focus'
import { cn } from '@/lib/utils'

interface DrawerProps {
  open: boolean
  onClose: () => void
  title: string
  /// 一句话说明这个抽屉在做什么。表单字段再清楚，也不如先告诉用户"这一步是干什么的"。
  description?: string
  /// 底部固定操作区（保存/取消）。放在底部而非表单末尾，长表单滚动时按钮始终可达。
  footer?: React.ReactNode
  /// 宽度档：md 适合单列表单，lg 给带表格/图的抽屉（用户管理、路由诊断）。
  size?: 'md' | 'lg' | 'xl'
  children: React.ReactNode
}

const SIZE = { md: 'max-w-xl', lg: 'max-w-2xl', xl: 'max-w-4xl' } as const

/// 进场焦点：优先第一个可输入控件（多页签抽屉则是当前页签），而不是标题旁的关闭按钮。
const firstField = (root: HTMLElement) =>
  root.querySelector<HTMLElement>(
    'input:not([type=hidden]):not([disabled]), textarea:not([disabled]), select:not([disabled]), [role=tab][aria-selected=true]',
  )

/// 右侧抽屉：承载单条记录的新建与编辑。
///
/// 为什么不继续用内联表单：列表页原本把"新建表单 + 列表 + 展开式编辑器"堆在一屏，
/// 用户要先滚过一大片表单才看到数据，且编辑时的上下文（改的是哪一条）容易丢。
/// 抽屉把"浏览"与"编辑"分成两个层次：列表始终是列表，编辑浮在其上并明确标题指向对象。
///
/// portal 到 body + 锁 body 滚动：抽屉打开时页面底下的表格不该还能滚。
/// 打开即把焦点送进第一个可输入控件——建渠道的人手已经在键盘上了；焦点在层内循环，
/// 关闭后还给打开它的那个按钮（`useModalFocus`）。
export function Drawer({
  open,
  onClose,
  title,
  description,
  footer,
  size = 'md',
  children,
}: DrawerProps) {
  const { t } = useTranslation()
  const panel = useRef<HTMLElement>(null)
  useModalFocus(open, panel, firstField, onClose)

  if (!open) return null

  return createPortal(
    <div className="fixed inset-0 z-40 flex justify-end">
      <button
        type="button"
        aria-label={t('common:close')}
        className="absolute inset-0 bg-black/40 backdrop-blur-[2px] animate-fade-in"
        onClick={onClose}
      />
      <aside
        ref={panel}
        role="dialog"
        aria-modal="true"
        aria-label={title}
        className={cn(
          'relative z-10 flex h-dvh min-h-0 w-full flex-col border-l border-border bg-card shadow-drawer animate-slide-in-right',
          SIZE[size],
        )}
      >
        <header className="flex shrink-0 items-start justify-between gap-3 border-b border-border px-4 py-4 sm:px-6">
          <div className="flex min-w-0 flex-col gap-1">
            <h2 className="truncate text-base font-semibold">{title}</h2>
            {description !== undefined && (
              <p className="text-xs leading-5 text-muted-foreground">{description}</p>
            )}
          </div>
          <Button size="icon" variant="ghost" className="-mt-1 -mr-2 h-10 w-10 shrink-0" aria-label={t('common:close')} onClick={onClose}>
            <X className="h-4 w-4" />
          </Button>
        </header>

        <div className="min-h-0 flex-1 overflow-y-auto overscroll-contain px-4 py-5 [scrollbar-gutter:stable] sm:px-6">{children}</div>

        {footer !== undefined && (
          <footer className="flex shrink-0 flex-wrap items-center justify-end gap-3 border-t border-border bg-card px-4 pt-3 pb-[max(0.75rem,env(safe-area-inset-bottom))] shadow-[0_-4px_16px_-12px_var(--muted-fg)] sm:px-6 [&>button]:min-h-11 [&>button]:flex-1 sm:[&>button]:min-h-9 sm:[&>button]:flex-none">
            {footer}
          </footer>
        )}
      </aside>
    </div>,
    document.body,
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
    <section className="flex flex-col gap-3 border-t border-border py-4 first:border-t-0 first:pt-0 last:pb-0">
      <div className="flex flex-col gap-0.5">
        <h3 className="text-xs font-semibold tracking-wide text-muted-foreground uppercase">
          {title}
        </h3>
        {hint !== undefined && <p className="text-xs leading-5 text-muted-foreground/80">{hint}</p>}
      </div>
      {children}
    </section>
  )
}
