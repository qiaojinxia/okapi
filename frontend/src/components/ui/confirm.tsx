import { AlertTriangle } from 'lucide-react'
import { useCallback, useEffect, useRef, useState } from 'react'
import { createPortal } from 'react-dom'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { useModalFocus } from '@/hooks/use-modal-focus'

interface ConfirmRequest {
  /// 标题（已本地化的文案）。
  title: string
  /// 补充说明：讲清后果，而不是只问"确定吗"。
  description?: string
  /// 高危操作要求手输该串才放行（如渠道名/模型名），防手滑点穿。
  requireText?: string
  confirmLabel?: string
  /// 非破坏性确认（如"重投"）用主色按钮；缺省按删除处理。
  tone?: 'destructive' | 'default'
  onConfirm: () => void
}

/// 危险操作二次确认。
///
/// 为什么需要：删除渠道/模型/角色都是不可逆或影响面很大的动作，此前点即执行——
/// 列表里相邻两行的删除按钮只差几十像素，误点没有任何挽回机会。
///
/// 用 hook + 单实例弹层而非每处自己写 state：调用方只关心"确认后做什么"。
export function useConfirm() {
  const [req, setReq] = useState<ConfirmRequest | null>(null)
  const confirm = useCallback((r: ConfirmRequest) => setReq(r), [])
  const dialog = <ConfirmDialog req={req} onClose={() => setReq(null)} />
  return { confirm, dialog }
}

/// 进场焦点：要手输名称时给输入框，否则给"确认"按钮（回车即执行）。
const firstControl = (root: HTMLElement) =>
  root.querySelector<HTMLElement>('input:not([disabled])') ??
  root.querySelector<HTMLElement>('button:not([disabled])[data-confirm]')

function ConfirmDialog({ req, onClose }: { req: ConfirmRequest | null; onClose: () => void }) {
  const { t } = useTranslation()
  const [typed, setTyped] = useState('')
  const panel = useRef<HTMLDivElement>(null)
  // 确认框常开在抽屉之上：焦点必须关在这一层里，否则 Tab 会走回底下那张表单
  useModalFocus(req !== null, panel, firstControl)

  const needText = req !== null && req.requireText !== undefined && req.requireText !== ''
  const ready = req !== null && (!needText || typed.trim() === req.requireText)

  const close = useCallback(() => {
    setTyped('')
    onClose()
  }, [onClose])

  // Esc 取消；需手输名称时在输入框里回车即确认（无需手输时确认按钮自带焦点，
  // 原生回车触发点击，这里不再重复处理以免执行两次）
  useEffect(() => {
    if (req === null) return undefined
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') close()
      if (e.key === 'Enter' && needText && ready) {
        e.preventDefault()
        req.onConfirm()
        close()
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [req, needText, ready, close])

  if (req === null) return null
  const destructive = (req.tone ?? 'destructive') === 'destructive'

  return createPortal(
    <div className="fixed inset-0 z-50 flex items-center justify-center p-4">
      <button
        type="button"
        aria-label={t('common:cancel')}
        className="absolute inset-0 bg-black/50 backdrop-blur-[2px] animate-fade-in"
        onClick={close}
      />
      <div
        ref={panel}
        role="alertdialog"
        aria-modal="true"
        aria-label={req.title}
        className="relative z-10 flex w-full max-w-md flex-col gap-4 rounded-xl border border-border bg-card p-5 shadow-popover animate-zoom-in"
      >
        <div className="flex items-start gap-3">
          <span
            className={
              destructive
                ? 'flex h-9 w-9 shrink-0 items-center justify-center rounded-full bg-destructive/12 text-destructive'
                : 'flex h-9 w-9 shrink-0 items-center justify-center rounded-full bg-primary/10 text-primary'
            }
          >
            <AlertTriangle className="h-4 w-4" />
          </span>
          <div className="flex min-w-0 flex-col gap-1 pt-1">
            <h2 className="text-sm font-semibold break-words">{req.title}</h2>
            {req.description !== undefined && (
              <p className="text-xs leading-5 text-muted-foreground">{req.description}</p>
            )}
          </div>
        </div>

        {needText && (
          <div className="flex flex-col gap-1.5">
            <label htmlFor="confirm-text" className="text-xs text-muted-foreground">
              {t('common:confirmTypeHint', { text: req.requireText })}
            </label>
            <Input
              id="confirm-text"
              value={typed}
              autoFocus
              autoComplete="off"
              className="font-mono text-xs"
              onChange={(e) => setTyped(e.target.value)}
            />
          </div>
        )}

        <div className="flex justify-end gap-2">
          <Button variant="ghost" size="sm" onClick={close}>
            {t('common:cancel')}
          </Button>
          <Button
            data-confirm
            variant={destructive ? 'destructive' : 'default'}
            size="sm"
            disabled={!ready}
            autoFocus={!needText}
            onClick={() => {
              req.onConfirm()
              close()
            }}
          >
            {req.confirmLabel ?? t('common:delete')}
          </Button>
        </div>
      </div>
    </div>,
    document.body,
  )
}
