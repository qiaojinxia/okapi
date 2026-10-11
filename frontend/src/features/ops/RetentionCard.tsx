import { useMutation, useQuery } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { toast } from '@/components/ui/toast'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Input, Label } from '@/components/ui/input'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { qk } from '@/lib/query-keys'
import { useConfirm } from '@/components/ui/confirm'
import { parseNonNegativeInt } from '@/lib/int32'

/// 数据保留策略（#1790-1）：retention_months，0=永久；worker 裁剪超期 PG 月分区。
export function RetentionCard() {
  const { t } = useTranslation()
  const [months, setMonths] = useState('')
  const { confirm, dialog } = useConfirm()

  const current = useQuery({
    queryKey: qk.setting('retention_months'),
    queryFn: () => apiFetch<{ value: number | null }>('/admin/settings/retention_months'),
  })

  // 只收非负整数：以前 Number("abc") 是 NaN、序列化成 null，落库后清理任务按"永久"处理且每轮报错
  const parsed = parseNonNegativeInt(months, 1200)
  const save = useMutation({
    mutationFn: () =>
      apiFetch('/admin/settings', {
        method: 'POST',
        body: { key: 'retention_months', value: parsed },
      }),
    onSuccess: () => {
      toast.success(t('admin:saved'))
      void current.refetch()
    },
    onError: (err) => toast.error(describeError(err)),
  })

  const shrinking =
    parsed !== undefined &&
    parsed !== null &&
    current.data?.value !== null &&
    current.data?.value !== undefined &&
    current.data.value !== 0 &&
    parsed !== 0 &&
    parsed < current.data.value

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
              aria-invalid={parsed === null}
              onChange={(e) => setMonths(e.target.value)}
            />
          </div>
          <Button
            disabled={save.isPending || parsed === undefined || parsed === null}
            onClick={() => {
              // 缩短保留期意味着 worker 会真的删掉月分区，且不可恢复
              if (shrinking) {
                confirm({
                  title: t('admin:retentionShrinkTitle'),
                  description: t('admin:retentionShrinkHint', { months: parsed }),
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
        </div>
      </CardContent>
    </Card>
  )
}
