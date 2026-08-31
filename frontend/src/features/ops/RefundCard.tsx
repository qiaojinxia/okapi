import { useMutation } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Input, Label } from '@/components/ui/input'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'

/// 按日志退款（§5.3）：事件溯源冲销，账单/统计/余额三处口径自动一致且幂等。
/// 故重复提交同一 request_id 是安全的——后端返回 already_refunded 而非二次退款。
export function RefundCard() {
  const { t } = useTranslation()
  const [requestId, setRequestId] = useState('')
  const [reason, setReason] = useState('')
  const [msg, setMsg] = useState<string | null>(null)

  const refund = useMutation({
    mutationFn: () =>
      apiFetch<{ outcome: string; refunded_micro?: number }>('/admin/billing/refund', {
        method: 'POST',
        body: { request_id: requestId.trim(), reason: reason.trim() },
      }),
    onSuccess: (r) => {
      setMsg(t('admin:refundOutcome', { outcome: r.outcome }))
      setRequestId('')
    },
    onError: (err) => setMsg(describeError(err)),
  })

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('admin:refundTitle')}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-wrap items-end gap-3">
        <p className="w-full text-xs text-muted-foreground">{t('admin:refundHint')}</p>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="rid">{t('admin:refundRequestId')}</Label>
          <Input
            id="rid"
            className="w-80 font-mono text-xs"
            value={requestId}
            placeholder="00000000-0000-0000-0000-000000000000"
            onChange={(e) => setRequestId(e.target.value)}
          />
        </div>
        <div className="flex flex-1 flex-col gap-1.5">
          <Label htmlFor="rreason">{t('admin:refundReason')}</Label>
          <Input id="rreason" value={reason} onChange={(e) => setReason(e.target.value)} />
        </div>
        <Button
          disabled={refund.isPending || requestId.trim() === ''}
          onClick={() => refund.mutate()}
        >
          {t('admin:refund')}
        </Button>
        {msg !== null && <span className="text-xs text-muted-foreground">{msg}</span>}
      </CardContent>
    </Card>
  )
}
