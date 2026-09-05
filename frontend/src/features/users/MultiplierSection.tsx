import { useMutation } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { FieldGroup } from '@/components/ui/drawer'
import { Input, Label } from '@/components/ui/input'
import { toast } from '@/components/ui/toast'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'

/// 个人计价系数：计价链上与模型倍率、分组倍率并列的一个乘数
/// （`实收 = 有效token × 模型倍率 × 分组倍率 × **个人系数** × 命中规则`）。
///
/// 此前这一列在用户列表里一直显示着却永远是 ×1——后端根本没有写入路径。
export function MultiplierSection({
  userId,
  current,
  onDone,
}: {
  userId: number
  current: string
  onDone: () => void
}) {
  const { t } = useTranslation()
  const [value, setValue] = useState(current)
  const parsed = Number(value)
  // 与后端同一道闸：非数字 / 负数 / 离谱值都是手滑
  const invalid = value.trim() === '' || !Number.isFinite(parsed) || parsed < 0 || parsed > 1000

  const save = useMutation({
    mutationFn: () =>
      apiFetch(`/admin/users/${userId}/multiplier`, {
        method: 'POST',
        body: { multiplier: value.trim() },
      }),
    onSuccess: () => {
      toast.success(t('common:success'))
      onDone()
    },
    onError: (err) => toast.error(describeError(err)),
  })

  return (
    <FieldGroup title={t('admin:userMultiplier')} hint={t('admin:userMultiplierHint')}>
      <div className="flex flex-wrap items-end gap-2">
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="multiplier">{t('admin:userMultiplierLabel')}</Label>
          <Input
            id="multiplier"
            className="w-32 font-mono"
            inputMode="decimal"
            value={value}
            onChange={(e) => setValue(e.target.value)}
          />
        </div>
        <Button
          disabled={invalid || save.isPending || value.trim() === current}
          onClick={() => save.mutate()}
        >
          {t('common:save')}
        </Button>
        <span className="pb-2 text-xs text-muted-foreground">
          {t('admin:userMultiplierPreview', { v: invalid ? '—' : `×${parsed}` })}
        </span>
      </div>
    </FieldGroup>
  )
}
