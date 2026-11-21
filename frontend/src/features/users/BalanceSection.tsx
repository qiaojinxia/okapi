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
/// 格式差一个字符后端就 400。选中的日期含当天：按管理员本地时间当天结束提交，
/// 门户也按本地日期显示到期日；提交 UTC 零点会在东八区当天早上就清零。
/// 一次充值 / 扣减的幂等键：同一次操作的重复提交（双击、超时后重发）共用它，后端只动一次钱。
/// `crypto.randomUUID` 只在安全上下文里有（站点按 IP 走 http 时没有），`getRandomValues` 到处都有。
function newOperationKey(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(16))
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, '0')).join('')
}

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
  // 金额或备注改了就是另一笔操作，换新键；成功后也换
  const [operationKey, setOperationKey] = useState(newOperationKey)

  const credit = useMutation({
    mutationFn: () =>
      apiFetch<{ balance_after_micro: number }>(`/admin/users/${userId}/credit`, {
        method: 'POST',
        // 界面按 USD 填，提交前换成 micro（后端一律 micro-USD 整数）
        body: { amount_micro: Math.round((Number(amount) || 0) * 1_000_000), reason },
        headers: { 'Idempotency-Key': operationKey },
      }),
    onSuccess: (r) => {
      toast.success(`${t('admin:balanceAfter')} ${formatMoney(r.balance_after_micro, i18n.language)}`)
      setAmount('')
      setOperationKey(newOperationKey())
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
            onChange={(e) => { setAmount(e.target.value); setOperationKey(newOperationKey()) }}
          />
        </div>
        <div className="flex flex-1 flex-col gap-1.5">
          <Label htmlFor="reason">{t('admin:creditReason')}</Label>
          <Input id="reason" value={reason} onChange={(e) => { setReason(e.target.value); setOperationKey(newOperationKey()) }} />
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
              expiry === '' ? null : new Date(`${expiry}T23:59:59.999`).toISOString(),
            )
          }
        >
          {expiry === '' ? t('admin:clearExpiry') : t('common:save')}
        </Button>
      </div>
    </FieldGroup>
  )
}
