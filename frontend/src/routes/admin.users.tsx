import { useMutation } from '@tanstack/react-query'
import { createFileRoute } from '@tanstack/react-router'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Input, Label } from '@/components/ui/input'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatMoney } from '@/lib/money'

export const Route = createFileRoute('/admin/users')({
  component: UsersPage,
})

function UsersPage() {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const [msg, setMsg] = useState<string | null>(null)
  const [form, setForm] = useState({ user_id: '', amount_micro: '', reason: '' })

  const credit = useMutation({
    mutationFn: () =>
      apiFetch<{ balance_after_micro: number }>(`/admin/users/${form.user_id}/credit`, {
        method: 'POST',
        body: { amount_micro: Number(form.amount_micro), reason: form.reason },
      }),
    onSuccess: (data) =>
      setMsg(`${t('admin:balanceAfter')}: ${formatMoney(data.balance_after_micro, locale)}`),
    onError: (err) => setMsg(describeError(err)),
  })

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('admin:credit')}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        <div className="grid grid-cols-1 gap-3 sm:grid-cols-3">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="user_id">{t('admin:userId')}</Label>
            <Input
              id="user_id"
              inputMode="numeric"
              value={form.user_id}
              onChange={(e) => setForm((f) => ({ ...f, user_id: e.target.value }))}
            />
          </div>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="amount">{t('admin:creditAmount')}</Label>
            <Input
              id="amount"
              inputMode="numeric"
              value={form.amount_micro}
              onChange={(e) => setForm((f) => ({ ...f, amount_micro: e.target.value }))}
            />
          </div>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="reason">{t('admin:creditReason')}</Label>
            <Input
              id="reason"
              value={form.reason}
              onChange={(e) => setForm((f) => ({ ...f, reason: e.target.value }))}
            />
          </div>
        </div>
        <div className="flex items-center gap-3">
          <Button
            disabled={credit.isPending || !form.user_id || !form.amount_micro}
            onClick={() => credit.mutate()}
          >
            {t('admin:credit')}
          </Button>
          {msg && <span className="text-xs text-muted-foreground">{msg}</span>}
        </div>
      </CardContent>
    </Card>
  )
}
