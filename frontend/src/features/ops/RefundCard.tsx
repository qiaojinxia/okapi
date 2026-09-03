import dayjs from 'dayjs'
import { useMutation } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { ErrorState } from '@/components/ui/state'
import { toast } from '@/components/ui/toast'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Input, Label } from '@/components/ui/input'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatMoney } from '@/lib/money'
import { useConfirm } from '@/components/ui/confirm'

interface RecordPreview {
  request_id: string
  user_id: number
  username: string | null
  model: string
  status: number
  amount_micro: number
  prompt_tokens: number
  completion_tokens: number
  error_code: string | null
  created_at: string
  refundable: boolean
}

interface RefundResp {
  outcome: 'refunded' | 'already_refunded'
  refunded_micro?: number
  balance_after_micro?: number
}

/// 按日志退款（§5.3）：**先查后退**两步——此前是对着一个 UUID 盲按退款，
/// 管理员看不到退的是谁的哪笔、多少钱。现在先查出账单摘要核对无误再退，
/// 且三种结局分开说：已退款（幂等，非错误）/ 未扣费不可退 / id 不存在。
export function RefundCard() {
  const { t, i18n } = useTranslation()
  const [requestId, setRequestId] = useState('')
  const [reason, setReason] = useState('')
  const [preview, setPreview] = useState<RecordPreview | null>(null)
  const [lookupError, setLookupError] = useState<string | null>(null)
  const { confirm, dialog } = useConfirm()

  const lookup = useMutation({
    mutationFn: () =>
      apiFetch<RecordPreview>(`/admin/billing/record/${requestId.trim()}`),
    onSuccess: (r) => {
      setLookupError(null)
      setPreview(r)
    },
    onError: (err) => {
      setPreview(null)
      setLookupError(describeError(err))
    },
  })

  const refund = useMutation({
    mutationFn: () =>
      apiFetch<RefundResp>('/admin/billing/refund', {
        method: 'POST',
        body: { request_id: requestId.trim(), reason: reason.trim() },
      }),
    onSuccess: (r) => {
      if (r.outcome === 'refunded') {
        toast.success(
          t('admin:refundDone', {
            amount: formatMoney(r.refunded_micro ?? 0, i18n.language),
            balance: formatMoney(r.balance_after_micro ?? 0, i18n.language),
          }),
        )
      } else {
        toast.info(t('admin:refundAlready'))
      }
      // 本地同步预览状态，免得管理员以为"没退成"再点一次
      setPreview((p) => (p === null ? null : { ...p, status: 30, refundable: false }))
    },
    onError: (err) => toast.error(describeError(err)),
  })

  const statusBadge = (p: RecordPreview) => {
    if (p.status === 20) return <Badge variant="success">{t('admin:refundStatusOk')}</Badge>
    if (p.status === 30) return <Badge variant="muted">{t('admin:refundStatusDone')}</Badge>
    return <Badge variant="warning">{t('admin:refundStatusNotBilled')}</Badge>
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('admin:refundTitle')}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        {dialog}
        <p className="text-xs text-muted-foreground">{t('admin:refundHint')}</p>
        <div className="flex flex-wrap items-end gap-3">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="rid">{t('admin:refundRequestId')}</Label>
            <Input
              id="rid"
              className="w-80 font-mono text-xs"
              value={requestId}
              placeholder="00000000-0000-0000-0000-000000000000"
              onChange={(e) => {
                setRequestId(e.target.value)
                setPreview(null)
              }}
            />
          </div>
          <Button
            variant="outline"
            disabled={lookup.isPending || requestId.trim() === ''}
            onClick={() => lookup.mutate()}
          >
            {t('admin:refundLookup')}
          </Button>
        </div>

        {preview !== null && (
          <div className="flex flex-col gap-2 rounded-md border border-border p-3">
            <div className="flex flex-wrap items-center gap-2 text-sm">
              {statusBadge(preview)}
              <Badge variant="muted">
                {preview.username ?? '?'} #{preview.user_id}
              </Badge>
              <span className="font-mono text-xs">{preview.model}</span>
              <span className="font-medium">
                {formatMoney(preview.amount_micro, i18n.language)}
              </span>
              <span className="text-xs text-muted-foreground">
                {dayjs(preview.created_at).format('YYYY-MM-DD HH:mm:ss')}
              </span>
              <span className="text-xs text-muted-foreground">
                {t('admin:refundTokens', {
                  p: preview.prompt_tokens,
                  c: preview.completion_tokens,
                })}
              </span>
              {preview.error_code !== null && (
                <Badge variant="destructive">{preview.error_code}</Badge>
              )}
            </div>
            <div className="flex flex-wrap items-end gap-3">
              <div className="flex flex-1 flex-col gap-1.5">
                <Label htmlFor="rreason">{t('admin:refundReason')}</Label>
                <Input id="rreason" value={reason} onChange={(e) => setReason(e.target.value)} />
              </div>
              <Button
                disabled={refund.isPending || !preview.refundable}
                onClick={() =>
                  confirm({
                    title: t('admin:refundTitle'),
                    description: t('admin:refundConfirm', {
                      user: preview.username ?? `#${preview.user_id}`,
                      amount: formatMoney(preview.amount_micro, i18n.language),
                    }),
                    confirmLabel: t('admin:refund'),
                    onConfirm: () => refund.mutate(),
                  })
                }
              >
                {t('admin:refund')}
              </Button>
            </div>
            {!preview.refundable && preview.status !== 30 && (
              <p className="text-xs text-muted-foreground">{t('admin:refundNotBilledHint')}</p>
            )}
          </div>
        )}

        {lookupError !== null && <ErrorState message={lookupError} />}
      </CardContent>
    </Card>
  )
}
