import { AlertTriangle } from 'lucide-react'
import { useCallback, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'

interface ConfirmRequest {
  /// 标题（已本地化的文案）。
  title: string
  /// 补充说明：讲清后果，而不是只问"确定吗"。
  description?: string
  /// 高危操作要求手输该串才放行（如渠道名/模型名），防手滑点穿。
  requireText?: string
  confirmLabel?: string
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

function ConfirmDialog({ req, onClose }: { req: ConfirmRequest | null; onClose: () => void }) {
  const { t } = useTranslation()
  const [typed, setTyped] = useState('')

  if (req === null) return null
  const needText = req.requireText !== undefined && req.requireText !== ''
  const ready = !needText || typed.trim() === req.requireText

  const close = () => {
    setTyped('')
    onClose()
  }

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center p-4">
      <button
        type="button"
        aria-label={t('common:cancel')}
        className="absolute inset-0 bg-black/50"
        onClick={close}
      />
      <div
        role="alertdialog"
        aria-modal="true"
        aria-label={req.title}
        className="relative z-10 flex w-full max-w-md flex-col gap-3 rounded-lg border border-border bg-card p-5 shadow-lg"
      >
        <div className="flex items-start gap-3">
          <AlertTriangle className="mt-0.5 h-5 w-5 shrink-0 text-destructive" />
          <div className="flex flex-col gap-1">
            <h2 className="text-sm font-semibold">{req.title}</h2>
            {req.description !== undefined && (
              <p className="text-xs text-muted-foreground">{req.description}</p>
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
            variant="destructive"
            size="sm"
            disabled={!ready}
            onClick={() => {
              req.onConfirm()
              close()
            }}
          >
            {req.confirmLabel ?? t('common:delete')}
          </Button>
        </div>
      </div>
    </div>
  )
}
