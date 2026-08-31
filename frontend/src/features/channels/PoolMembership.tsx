import { useMutation, useQuery } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import { FieldGroup } from '@/components/ui/drawer'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { qk } from '@/lib/query-keys'

/// 本渠道属于哪些渠道池。空集 = 不入池，即对所有人可见（宽松默认）。
///
/// `current` 必须由调用方按渠道当前成员关系传入：此前这里从空集起手，
/// 点一次保存就会把已有的池成员关系全部清掉。
export function PoolMembership({
  channelId,
  current,
  onDone,
  onMsg,
}: {
  channelId: number
  current: string[]
  onDone: () => void
  onMsg: (m: string) => void
}) {
  const { t } = useTranslation()
  const [picked, setPicked] = useState<Set<string>>(new Set(current))

  const pools = useQuery({
    queryKey: qk.adminPools,
    queryFn: () => apiFetch<{ data: { pool_code: string }[] }>('/admin/pools'),
  })

  const save = useMutation({
    mutationFn: () =>
      apiFetch(`/admin/channels/${channelId}/pools`, {
        method: 'POST',
        body: { pools: [...picked] },
      }),
    onSuccess: () => {
      onMsg(t('admin:poolsSaved'))
      onDone()
    },
    onError: (err) => onMsg(describeError(err)),
  })

  const list = pools.data?.data ?? []

  return (
    <FieldGroup title={t('admin:poolMembership')} hint={t('admin:poolMembershipHint')}>
      {list.length === 0 ? (
        <p className="text-xs text-muted-foreground">{t('admin:poolsEmptyHint')}</p>
      ) : (
        <div className="flex flex-wrap gap-3">
          {list.map((p) => (
            <Checkbox
              key={p.pool_code}
              label={p.pool_code}
              checked={picked.has(p.pool_code)}
              className="font-mono text-xs"
              onChange={() =>
                setPicked((prev) => {
                  const next = new Set(prev)
                  if (next.has(p.pool_code)) next.delete(p.pool_code)
                  else next.add(p.pool_code)
                  return next
                })
              }
            />
          ))}
        </div>
      )}
      <Button
        size="sm"
        variant="outline"
        className="self-start"
        disabled={save.isPending || list.length === 0}
        onClick={() => save.mutate()}
      >
        {t('admin:savePools')}
      </Button>
    </FieldGroup>
  )
}
