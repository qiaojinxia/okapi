import { useMutation } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Undo2 } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { useConfirm } from '@/components/ui/confirm'
import { Input } from '@/components/ui/input'
import { usePermission } from '@/hooks/use-auth'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatMoney } from '@/lib/money'
import type { LogRow, RefundResp } from './types'

/// 行内退款（§5.3 按日志退款，#1790-10）——运维页的退款卡要先去日志页复制
/// request_id 再回去粘贴；而管理员正是在看到这条日志时决定要退的，动作就该在这里。
/// 复用同一后端端点（幂等：重复提交回 already_refunded），
/// 只对"成功且扣了钱"的消费行显示；权限点 billing.refund 不足则整块不出现。
export function RefundInline({ row }: { row: LogRow }) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const can = usePermission()
  const { confirm, dialog } = useConfirm()
  const [open, setOpen] = useState(false)
  const [reason, setReason] = useState('')
  const [done, setDone] = useState<string | null>(null)

  const refund = useMutation({
    mutationFn: () =>
      apiFetch<RefundResp>('/admin/billing/refund', {
        method: 'POST',
        body: { request_id: row.request_id, reason: reason.trim() },
      }),
    onSuccess: (r) => {
      setDone(
        r.outcome === 'refunded'
          ? t('admin:refundDone', {
              amount: formatMoney(r.refunded_micro ?? 0, locale),
              balance: formatMoney(r.balance_after_micro ?? 0, locale),
            })
          : t('admin:refundAlready'),
      )
      setOpen(false)
    },
    onError: (err) => setDone(describeError(err)),
  })

  if (!can('billing.refund') || row.is_error || row.log_type !== 2 || row.amount_micro <= 0) {
    return null
  }
  if (done !== null) {
    return <span className="text-xs text-muted-foreground">{done}</span>
  }
  return (
    <div className="flex flex-wrap items-center gap-2">
      {dialog}
      {open ? (
        <>
          <Input
            className="h-7 w-64 text-xs"
            placeholder={t('admin:refundReason')}
            value={reason}
            onChange={(e) => setReason(e.target.value)}
          />
          <Button
            size="sm"
            variant="destructive"
            disabled={refund.isPending}
            onClick={() =>
              confirm({
                title: t('admin:refundTitle'),
                description: t('admin:refundConfirm', {
                  user: row.username || `#${row.user_id}`,
                  amount: formatMoney(row.amount_micro, locale),
                }),
                confirmLabel: t('admin:refund'),
                onConfirm: () => refund.mutate(),
              })
            }
          >
            {t('admin:refund')}
          </Button>
          <Button size="sm" variant="ghost" onClick={() => setOpen(false)}>
            {t('common:cancel')}
          </Button>
        </>
      ) : (
        <Button size="sm" variant="outline" onClick={() => setOpen(true)}>
          <Undo2 className="mr-1 h-3.5 w-3.5" />
          {t('admin:refund')}
        </Button>
      )}
    </div>
  )
}
