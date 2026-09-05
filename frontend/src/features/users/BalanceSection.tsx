import { useMutation } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { FieldGroup } from '@/components/ui/drawer'
import { Input, Label } from '@/components/ui/input'
import { toast } from '@/components/ui/toast'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatMoney } from '@/lib/money'

/// 余额分区：充值/扣减 + 余额有效期。
///
/// 有效期用日期选择器而非手写 ISO8601——此前占位符是 2026-12-31T00:00:00Z，
/// 格式差一个字符后端就 400。
export function BalanceSection({
  userId,
  onDone,
}: {
  userId: number
  onDone: () => void
}) {
  const { t, i18n } = useTranslation()
  const [amount, setAmount] = useState('')
  const [reason, setReason] = useState('')
  const [expiry, setExpiry] = useState('')

  const credit = useMutation({
    mutationFn: () =>
      apiFetch<{ balance_after_micro: number }>(`/admin/users/${userId}/credit`, {
        method: 'POST',
        // 界面按 USD 填，提交前换成 micro（后端一律 micro-USD 整数）
        body: { amount_micro: Math.round((Number(amount) || 0) * 1_000_000), reason },
      }),
    onSuccess: (r) => {
      toast.success(`${t('admin:balanceAfter')} ${formatMoney(r.balance_after_micro, i18n.language)}`)
      setAmount('')
      onDone()
    },
    onError: (err) => toast.error(describeError(err)),
  })

  const setBalanceExpiry = useMutation({
    mutationFn: (expires_at: string | null) =>
      apiFetch(`/admin/users/${userId}/balance-expiry`, {
        method: 'POST',
        // null 表示取消有效期（永不过期）
        body: { expires_at },
      }),
    onSuccess: () => {
      toast.success(t('common:success'))
      onDone()
    },
    onError: (err) => toast.error(describeError(err)),
  })

  return (
    <FieldGroup title={t('common:balance')} hint={t('admin:creditHint')}>
      <div className="flex flex-wrap items-end gap-2">
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="amt">{t('admin:creditUsd')}</Label>
          <Input
            id="amt"
            className="w-28"
            inputMode="decimal"
            placeholder="10 / -10"
            value={amount}
            onChange={(e) => setAmount(e.target.value)}
          />
        </div>
        <div className="flex flex-1 flex-col gap-1.5">
          <Label htmlFor="reason">{t('admin:creditReason')}</Label>
          <Input id="reason" value={reason} onChange={(e) => setReason(e.target.value)} />
        </div>
        <Button size="sm" disabled={credit.isPending || amount.trim() === ''} onClick={() => credit.mutate()}>
          {t('admin:credit')}
        </Button>
      </div>

      <div className="flex flex-wrap items-end gap-2">
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="uexpiry">{t('admin:balanceExpiry')}</Label>
          <Input
            id="uexpiry"
            type="date"
            className="w-44"
            value={expiry}
            onChange={(e) => setExpiry(e.target.value)}
          />
        </div>
        <Button
          size="sm"
          variant="outline"
          onClick={() =>
            setBalanceExpiry.mutate(
              expiry === '' ? null : new Date(`${expiry}T00:00:00Z`).toISOString(),
            )
          }
        >
          {expiry === '' ? t('admin:clearExpiry') : t('common:save')}
        </Button>
      </div>
    </FieldGroup>
  )
}
