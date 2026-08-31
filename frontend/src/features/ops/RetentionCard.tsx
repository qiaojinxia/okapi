import { useMutation, useQuery } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Input, Label } from '@/components/ui/input'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { useConfirm } from '@/components/ui/confirm'

/// 数据保留策略（#1790-1）：retention_months，0=永久；worker 裁剪超期 PG 月分区。
export function RetentionCard() {
  const { t } = useTranslation()
  const [months, setMonths] = useState('')
  const [msg, setMsg] = useState<string | null>(null)
  const { confirm, dialog } = useConfirm()

  const current = useQuery({
    queryKey: ['setting', 'retention_months'],
    queryFn: () => apiFetch<{ value: number | null }>('/admin/settings/retention_months'),
  })

  const save = useMutation({
    mutationFn: () =>
      apiFetch('/admin/settings', {
        method: 'POST',
        body: { key: 'retention_months', value: Number(months) },
      }),
    onSuccess: () => {
      setMsg(t('admin:saved'))
      void current.refetch()
    },
    onError: (err) => setMsg(describeError(err)),
  })

  const shrinking =
    months !== '' &&
    current.data?.value !== null &&
    current.data?.value !== undefined &&
    current.data.value !== 0 &&
    Number(months) !== 0 &&
    Number(months) < current.data.value

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('admin:retention')}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        {dialog}
        <p className="text-xs text-muted-foreground">
          {t('admin:retentionHint', { current: current.data?.value ?? 0 })}
        </p>
        <div className="flex items-end gap-3">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="retention">{t('admin:retentionMonths')}</Label>
            <Input
              id="retention"
              inputMode="numeric"
              className="w-40"
              value={months}
              placeholder="0"
              onChange={(e) => setMonths(e.target.value)}
            />
          </div>
          <Button
            disabled={save.isPending || months === ''}
            onClick={() => {
              // 缩短保留期意味着 worker 会真的删掉月分区，且不可恢复
              if (shrinking) {
                confirm({
                  title: t('admin:retentionShrinkTitle'),
                  description: t('admin:retentionShrinkHint', { months: Number(months) }),
                  confirmLabel: t('common:save'),
                  onConfirm: () => save.mutate(),
                })
                return
              }
              save.mutate()
            }}
          >
            {t('common:save')}
          </Button>
          {msg !== null && <span className="pb-2 text-xs text-muted-foreground">{msg}</span>}
        </div>
      </CardContent>
    </Card>
  )
}
