import { Plus, Trash2 } from 'lucide-react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { toast } from '@/components/ui/toast'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Checkbox } from '@/components/ui/checkbox'
import { EVENT_LABEL, NOTIFY_EVENTS } from '@/features/settings/types'
import { EmptyState } from '@/components/ui/state'
import { IconButton } from '@/components/ui/icon-button'
import { Input, Label } from '@/components/ui/input'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'

export interface NotifyChannel {
  type: string
  url: string
  events: string[]
  min_interval_secs: number
}


/// 通知渠道配置（#1790-8）。
///
/// 此前是一个 JSON 数组文本框，用户得自己拼 `[{"type":"webhook","url":...,"events":[...]}]`——
/// 事件名拼错不会报错，只是永远收不到通知。改成每路一行的结构化编辑。
export function NotifyCard() {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const [rows, setRows] = useState<NotifyChannel[] | null>(null)

  const current = useQuery({
    queryKey: ['setting', 'notify_channels'],
    queryFn: () => apiFetch<{ value: unknown }>('/admin/settings/notify_channels'),
  })

  const loaded: NotifyChannel[] = Array.isArray(current.data?.value)
    ? (current.data.value as NotifyChannel[])
    : []
  const list = rows ?? loaded

  const save = useMutation({
    mutationFn: () =>
      apiFetch('/admin/settings', {
        method: 'POST',
        body: { key: 'notify_channels', value: list },
      }),
    onSuccess: () => {
      toast.success(t('admin:saved'))
      setRows(null)
      void current.refetch()
      void queryClient.invalidateQueries({ queryKey: ['admin', 'settings'] })
    },
    onError: (err) => toast.error(describeError(err)),
  })

  const patch = (i: number, next: Partial<NotifyChannel>) =>
    setRows(list.map((r, j) => (i === j ? { ...r, ...next } : r)))

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('admin:notify')}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        <p className="text-xs text-muted-foreground">{t('admin:notifyHint')}</p>

        {list.length === 0 && <EmptyState hint={t('admin:notifyEmptyHint')} />}

        {list.map((row, i) => (
          <div key={i} className="flex flex-col gap-2 rounded-md border border-border p-3">
            <div className="flex items-end gap-2">
              <div className="flex flex-1 flex-col gap-1.5">
                <Label htmlFor={`nurl-${i}`}>{t('admin:notifyUrl')}</Label>
                <Input
                  id={`nurl-${i}`}
                  className="font-mono text-xs"
                  value={row.url}
                  placeholder="https://hooks.example.com/..."
                  onChange={(e) => patch(i, { url: e.target.value })}
                />
              </div>
              <div className="flex w-32 flex-col gap-1.5">
                <Label htmlFor={`nint-${i}`}>{t('admin:notifyInterval')}</Label>
                <Input
                  id={`nint-${i}`}
                  inputMode="numeric"
                  value={String(row.min_interval_secs)}
                  onChange={(e) => patch(i, { min_interval_secs: Number(e.target.value) || 0 })}
                />
              </div>
              <IconButton
                icon={Trash2}
                label={t('common:delete')}
                variant="destructive"
                onClick={() => setRows(list.filter((_, j) => j !== i))}
              />
            </div>
            <div className="flex flex-wrap gap-3">
              {NOTIFY_EVENTS.map((ev) => (
                <Checkbox
                  key={ev}
                  label={t(EVENT_LABEL[ev])}
                  checked={row.events.includes(ev)}
                  onChange={(on) =>
                    patch(i, {
                      events: on ? [...row.events, ev] : row.events.filter((e) => e !== ev),
                    })
                  }
                />
              ))}
            </div>
          </div>
        ))}

        <div className="flex items-center gap-2">
          <Button
            size="sm"
            variant="outline"
            onClick={() =>
              setRows([
                ...list,
                { type: 'webhook', url: '', events: [...NOTIFY_EVENTS], min_interval_secs: 300 },
              ])
            }
          >
            <Plus className="h-4 w-4" />
            {t('admin:notifyAdd')}
          </Button>
          <Button size="sm" disabled={save.isPending} onClick={() => save.mutate()}>
            {t('common:save')}
          </Button>
        </div>
      </CardContent>
    </Card>
  )
}
