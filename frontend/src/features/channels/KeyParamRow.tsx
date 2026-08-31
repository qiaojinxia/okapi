import { useMutation } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { ChannelKeyRow } from '@/features/channels/types'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Input, Label } from '@/components/ui/input'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'

/// 单把 key 的权重与并发上限（加权随机的调度参数）。
/// 并发上限走三态语义：留空提交 null = 解除上限，与"不改"区分。
export function KeyParamRow({
  channelId,
  row,
  onDone,
  onMsg,
}: {
  channelId: number
  row: ChannelKeyRow
  onDone: () => void
  onMsg: (m: string) => void
}) {
  const { t } = useTranslation()
  const [weight, setWeight] = useState(String(row.weight))
  const [conc, setConc] = useState(row.max_concurrency === null ? '' : String(row.max_concurrency))

  const save = useMutation({
    mutationFn: () =>
      apiFetch(`/admin/channels/${channelId}/keys/${row.id}`, {
        method: 'PATCH',
        body: {
          weight: Number(weight) || 0,
          max_concurrency: conc.trim() === '' ? null : Number(conc),
        },
      }),
    onSuccess: () => {
      onMsg(t('common:success'))
      onDone()
    },
    onError: (err) => onMsg(describeError(err)),
  })

  return (
    <div className="flex flex-wrap items-end gap-2 rounded-md border border-border p-2">
      <Badge variant={row.status === 1 ? 'success' : 'muted'}>#{row.id}</Badge>
      <div className="flex flex-col gap-1.5">
        <Label htmlFor={`kw-${row.id}`}>{t('admin:keyWeight')}</Label>
        <Input
          id={`kw-${row.id}`}
          className="w-20"
          value={weight}
          inputMode="numeric"
          onChange={(e) => setWeight(e.target.value)}
        />
      </div>
      <div className="flex flex-col gap-1.5">
        <Label htmlFor={`kc-${row.id}`}>{t('admin:keyConcurrency')}</Label>
        <Input
          id={`kc-${row.id}`}
          className="w-24"
          value={conc}
          placeholder={t('team:noLimit')}
          inputMode="numeric"
          onChange={(e) => setConc(e.target.value)}
        />
      </div>
      <Button size="sm" variant="outline" disabled={save.isPending} onClick={() => save.mutate()}>
        {t('common:save')}
      </Button>
      {row.cooldown_until !== null && (
        <span className="text-xs text-destructive">
          {t('admin:keyCooling', { until: row.cooldown_until })}
        </span>
      )}
      {row.failed_count > 0 && (
        <span className="text-xs text-muted-foreground">
          {t('admin:keyFails', { n: row.failed_count })}
        </span>
      )}
    </div>
  )
}
