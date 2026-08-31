import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { createFileRoute } from '@tanstack/react-router'
import dayjs from 'dayjs'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Input, Label } from '@/components/ui/input'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatMoney } from '@/lib/money'
import { qk } from '@/lib/query-keys'

export const Route = createFileRoute('/admin/keys')({
  component: AdminKeysPage,
})

interface AdminKeyRow {
  id: number
  user_id: number
  username: string
  team_id: number | null
  name: string
  key_prefix: string
  status: number
  quota_mode: number
  quota_micro: number | null
  used_micro: number
  model_allowlist: string[] | null
  group_override: string | null
  rpm_limit: number | null
  max_concurrency: number | null
  expires_at: string | null
  last_used_at: string | null
  created_at: string
}

/// 令牌管理面：跨用户排查与处置（停用/删除）。
/// 与门户自助页的区别是可跨用户检索——排查滥用时按用户名/令牌名定位。
function AdminKeysPage() {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const queryClient = useQueryClient()
  const [search, setSearch] = useState('')
  const [query, setQuery] = useState('')
  const [userId, setUserId] = useState('')
  const [msg, setMsg] = useState<string | null>(null)

  const uid = userId.trim() === '' ? null : Number(userId)
  const keys = useQuery({
    queryKey: qk.adminKeys(uid, query),
    queryFn: () => {
      const params = new URLSearchParams({ limit: '100' })
      if (query !== '') params.set('q', query)
      if (uid !== null && Number.isFinite(uid)) params.set('user_id', String(uid))
      return apiFetch<{ total: number; data: AdminKeyRow[] }>(`/admin/keys?${params}`)
    },
  })

  const invalidate = () => void queryClient.invalidateQueries({ queryKey: ['admin', 'keys'] })

  const setStatus = useMutation({
    mutationFn: (arg: { id: number; status: number }) =>
      apiFetch(`/admin/keys/${arg.id}`, { method: 'PATCH', body: { status: arg.status } }),
    onSuccess: () => {
      setMsg(t('common:success'))
      invalidate()
    },
    onError: (err) => setMsg(describeError(err)),
  })

  const remove = useMutation({
    mutationFn: (id: number) => apiFetch(`/admin/keys/${id}`, { method: 'DELETE' }),
    onSuccess: () => {
      setMsg(t('common:success'))
      invalidate()
    },
    onError: (err) => setMsg(describeError(err)),
  })

  return (
    <div className="flex flex-col gap-4">
      <Card>
        <CardContent className="flex flex-wrap items-end gap-3 py-3">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="kq">{t('admin:keySearch')}</Label>
            <Input
              id="kq"
              value={search}
              placeholder={t('admin:keySearchHint')}
              onChange={(e) => setSearch(e.target.value)}
            />
          </div>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="kuid">{t('admin:keyFilterUser')}</Label>
            <Input
              id="kuid"
              className="w-28"
              value={userId}
              inputMode="numeric"
              onChange={(e) => setUserId(e.target.value)}
            />
          </div>
          <Button size="sm" onClick={() => setQuery(search.trim())}>
            {t('admin:usersSearch')}
          </Button>
          <span className="text-xs text-muted-foreground">
            {t('admin:keyTotal', { n: keys.data?.total ?? 0 })}
          </span>
          {msg !== null && <span className="text-xs text-muted-foreground">{msg}</span>}
        </CardContent>
      </Card>

      {keys.isError ? (
        <p className="text-sm text-destructive">{describeError(keys.error)}</p>
      ) : (
        <Table>
          <THead>
            <Tr>
              <Th>ID</Th>
              <Th>{t('admin:username')}</Th>
              <Th>{t('portal:keyName')}</Th>
              <Th>{t('portal:keyPrefix')}</Th>
              <Th>{t('common:status')}</Th>
              <Th>{t('portal:keyUsed')}</Th>
              <Th>{t('portal:keyRpm')}</Th>
              <Th>{t('admin:keyLastUsed')}</Th>
              <Th>{t('common:actions')}</Th>
            </Tr>
          </THead>
          <TBody>
            {(keys.data?.data ?? []).map((k) => (
              <Tr key={k.id}>
                <Td>{k.id}</Td>
                <Td>{k.username}</Td>
                <Td>{k.name}</Td>
                <Td className="font-mono text-xs">{k.key_prefix}…</Td>
                <Td>
                  <Badge variant={k.status === 1 ? 'success' : 'muted'}>
                    {k.status === 1 ? t('common:enabled') : t('common:disabled')}
                  </Badge>
                </Td>
                <Td>{formatMoney(k.used_micro, locale)}</Td>
                <Td>{k.rpm_limit ?? '—'}</Td>
                <Td>{k.last_used_at ? dayjs(k.last_used_at).format('MM-DD HH:mm') : '—'}</Td>
                <Td className="flex flex-wrap gap-1.5">
                  <Button
                    size="sm"
                    variant="outline"
                    onClick={() => setStatus.mutate({ id: k.id, status: k.status === 1 ? 2 : 1 })}
                  >
                    {k.status === 1 ? t('common:disabled') : t('common:enabled')}
                  </Button>
                  <Button size="sm" variant="destructive" onClick={() => remove.mutate(k.id)}>
                    {t('common:delete')}
                  </Button>
                </Td>
              </Tr>
            ))}
          </TBody>
        </Table>
      )}
    </div>
  )
}
