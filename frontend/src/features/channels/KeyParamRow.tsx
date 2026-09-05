import { useMutation } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { ChannelKeyRow } from '@/features/channels/types'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Input, Label } from '@/components/ui/input'
import { toast } from '@/components/ui/toast'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'

/// 单把 key 的权重、并发上限与状态。
///
/// 并发上限走三态语义：留空提交 null = 解除上限，与"不改"区分。
///
/// "重新启用"是唯一能把被上游 401/403 打成失效（status=6）的 key 拉回可用的入口——
/// 那个状态没有冷却、恢复 worker 也不捞它，此前只能靠重置凭证救。
export function KeyParamRow({
  channelId,
  row,
  onDone,
}: {
  channelId: number
  row: ChannelKeyRow
  onDone: () => void
}) {
  const { t } = useTranslation()
  const [weight, setWeight] = useState(String(row.weight))
  const [conc, setConc] = useState(row.max_concurrency === null ? '' : String(row.max_concurrency))

  const enable = useMutation({
    mutationFn: () =>
      apiFetch(`/admin/channels/${channelId}/keys/${row.id}`, {
        method: 'PATCH',
        body: { status: 1 },
      }),
    onSuccess: () => {
      toast.success(t('common:success'))
      onDone()
    },
    onError: (err) => toast.error(describeError(err)),
  })

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
      toast.success(t('common:success'))
      onDone()
    },
    onError: (err) => toast.error(describeError(err)),
  })

  return (
    <div className="flex flex-wrap items-end gap-2 rounded-md border border-border p-2">
      <div className="flex h-9 items-center">
        <Badge variant={row.status === 1 ? 'success' : row.status === 6 ? 'destructive' : 'muted'}>
          #{row.id}
        </Badge>
      </div>
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
      <div className="flex h-9 items-center">
        <Button size="sm" variant="outline" disabled={save.isPending} onClick={() => save.mutate()}>
          {t('common:save')}
        </Button>
      </div>
      {row.status !== 1 && (
        <div className="flex h-9 items-center">
          <Button
            size="sm"
            variant="outline"
            disabled={enable.isPending}
            onClick={() => enable.mutate()}
          >
            {t('admin:keyReEnable')}
          </Button>
        </div>
      )}
      {row.status === 6 && (
        <span className="flex h-9 items-center text-xs text-destructive">
          {t('admin:keyInvalid')}
        </span>
      )}
      {row.cooldown_until !== null && (
        <span className="flex h-9 items-center text-xs text-destructive">
          {t('admin:keyCooling', { until: row.cooldown_until })}
        </span>
      )}
      {row.failed_count > 0 && (
        <span className="flex h-9 items-center text-xs text-muted-foreground">
          {t('admin:keyFails', { n: row.failed_count })}
        </span>
      )}
    </div>
  )
}
