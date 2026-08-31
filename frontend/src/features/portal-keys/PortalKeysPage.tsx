import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import dayjs from 'dayjs'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { useConfirm } from '@/components/ui/confirm'
import { Input, Label } from '@/components/ui/input'
import { EmptyState, ErrorState } from '@/components/ui/state'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatCount, formatMoney } from '@/lib/money'
import { qk } from '@/lib/query-keys'


interface KeyRow {
  id: number
  name: string
  key_prefix: string
  status: number
  used_micro: number
  rpm_limit: number | null
  created_at: string
  amount_micro: number
  requests: number
}

export function PortalKeysPage() {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const queryClient = useQueryClient()
  const [msg, setMsg] = useState<string | null>(null)
  const [renaming, setRenaming] = useState<{ id: number; name: string } | null>(null)
  const { confirm, dialog } = useConfirm()

  const keys = useQuery({
    queryKey: qk.keys,
    queryFn: () => apiFetch<{ data: KeyRow[] }>('/api/me/keys'),
  })
  const invalidate = () => void queryClient.invalidateQueries({ queryKey: qk.keys })

  const patch = useMutation({
    mutationFn: (arg: { id: number; body: Record<string, unknown> }) =>
      apiFetch(`/api/me/keys/${arg.id}`, { method: 'PATCH', body: arg.body }),
    onSuccess: () => {
      setMsg(t('common:success'))
      setRenaming(null)
      invalidate()
    },
    onError: (err) => setMsg(describeError(err)),
  })

  const remove = useMutation({
    mutationFn: (id: number) => apiFetch(`/api/me/keys/${id}`, { method: 'DELETE' }),
    onSuccess: () => {
      setMsg(t('common:success'))
      invalidate()
    },
    onError: (err) => setMsg(describeError(err)),
  })

  if (keys.isError) {
    return <ErrorState message={describeError(keys.error)} />
  }
  const rows = keys.data?.data ?? []
  return (
    <div className="flex flex-col gap-3">
      {dialog}
      {msg !== null && <p className="text-xs text-muted-foreground">{msg}</p>}
      {renaming !== null && (
        <div className="flex flex-wrap items-end gap-3">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="rename">{t('portal:keyName')}</Label>
            <Input
              id="rename"
              value={renaming.name}
              onChange={(e) => setRenaming({ ...renaming, name: e.target.value })}
            />
          </div>
          <Button
            size="sm"
            disabled={renaming.name.trim() === ''}
            onClick={() => patch.mutate({ id: renaming.id, body: { name: renaming.name.trim() } })}
          >
            {t('common:save')}
          </Button>
          <Button size="sm" variant="ghost" onClick={() => setRenaming(null)}>
            {t('common:cancel')}
          </Button>
        </div>
      )}
      <Table>
        <THead>
          <Tr>
            <Th>{t('portal:keyName')}</Th>
            <Th>{t('portal:keyPrefix')}</Th>
            <Th>{t('common:status')}</Th>
            <Th>{t('portal:keyUsed')}</Th>
            <Th>{t('portal:keyRequests')}</Th>
            <Th>{t('portal:keyRpm')}</Th>
            <Th>{t('portal:keyCreated')}</Th>
            <Th>{t('common:actions')}</Th>
          </Tr>
        </THead>
        <TBody>
          {rows.map((k) => (
            <Tr key={k.id}>
              <Td>{k.name}</Td>
              <Td className="font-mono text-xs">{k.key_prefix}…</Td>
              <Td>
                <Badge variant={k.status === 1 ? 'success' : 'muted'}>
                  {k.status === 1 ? t('common:enabled') : t('common:disabled')}
                </Badge>
              </Td>
              <Td>{formatMoney(k.amount_micro || k.used_micro, locale)}</Td>
              <Td>{formatCount(k.requests, locale)}</Td>
              <Td>{k.rpm_limit ?? '—'}</Td>
              <Td>{dayjs(k.created_at).format('YYYY-MM-DD')}</Td>
              <Td className="flex flex-wrap gap-1.5">
                <Button
                  size="sm"
                  variant="outline"
                  onClick={() =>
                    patch.mutate({ id: k.id, body: { status: k.status === 1 ? 2 : 1 } })
                  }
                >
                  {k.status === 1 ? t('common:disabled') : t('common:enabled')}
                </Button>
                <Button
                  size="sm"
                  variant="outline"
                  onClick={() => setRenaming({ id: k.id, name: k.name })}
                >
                  {t('common:edit')}
                </Button>
                <Button
                  size="sm"
                  variant="destructive"
                  onClick={() =>
                    confirm({
                      title: t('common:confirmDeleteTitle', { name: k.name }),
                      description: t('common:confirmKeyDelete'),
                      requireText: k.name,
                      onConfirm: () => remove.mutate(k.id),
                    })
                  }
                >
                  {t('common:delete')}
                </Button>
              </Td>
            </Tr>
          ))}
        </TBody>
      </Table>
      {rows.length === 0 && <EmptyState hint={t('portal:keysEmptyHint')} />}
    </div>
  )
}
