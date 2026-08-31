import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { createFileRoute } from '@tanstack/react-router'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Input, Label } from '@/components/ui/input'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatMoney } from '@/lib/money'
import { qk } from '@/lib/query-keys'

export const Route = createFileRoute('/admin/users')({
  component: UsersPage,
})

/// 内置角色三档（后端 assign_role 只接受 1 / 10 / 100）。
const BUILTIN_ROLES = [1, 10, 100] as const

interface UserRow {
  id: number
  username: string
  email: string | null
  role: number
  status: number
  balance_micro: number
  admin_role_id: number | null
  price_multiplier: string
}

interface RoleRow {
  id: number
  role_code: string
  display_name: string
  permissions: unknown
}

interface Overview {
  user: { id: number; username: string; role: number; status: number; balance_micro: number }
  groups: { code: string; priority: number }[]
  keys: { id: number; name: string; key_prefix: string; status: number; used_micro: number }[]
}

function roleLabel(role: number, t: (k: string) => string): string {
  if (role >= 100) return t('admin:roleSuper')
  if (role >= 10) return t('admin:roleAdmin')
  return t('admin:roleUser')
}

function UsersPage() {
  const [selected, setSelected] = useState<number | null>(null)
  return (
    <div className="flex flex-col gap-4">
      <UserListCard selected={selected} onSelect={setSelected} />
      {selected !== null && <UserActionsCard userId={selected} />}
      <RolesCard />
    </div>
  )
}

function UserListCard({
  selected,
  onSelect,
}: {
  selected: number | null
  onSelect: (id: number) => void
}) {
  const { t, i18n } = useTranslation()
  const [search, setSearch] = useState('')
  const [query, setQuery] = useState('')

  const users = useQuery({
    queryKey: qk.adminUsers(query),
    queryFn: () =>
      apiFetch<{ total: number; data: UserRow[] }>(
        `/admin/users?limit=50${query ? `&q=${encodeURIComponent(query)}` : ''}`,
      ),
  })

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('admin:usersTitle')}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        <div className="flex flex-wrap items-end gap-3">
          <div className="flex min-w-64 flex-col gap-1.5">
            <Label htmlFor="q">{t('admin:usersSearch')}</Label>
            <Input
              id="q"
              value={search}
              onChange={(e) => setSearch(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter') setQuery(search.trim())
              }}
            />
          </div>
          <Button variant="outline" onClick={() => setQuery(search.trim())}>
            {t('common:refresh')}
          </Button>
          <span className="text-xs text-muted-foreground">
            {t('admin:usersTotal', { total: users.data?.total ?? 0 })}
          </span>
        </div>

        {users.isError ? (
          <p className="text-sm text-destructive">{describeError(users.error)}</p>
        ) : (
          <Table>
            <THead>
              <Tr>
                <Th>ID</Th>
                <Th>{t('admin:username')}</Th>
                <Th>{t('admin:usersEmail')}</Th>
                <Th>{t('admin:usersRole')}</Th>
                <Th>{t('common:status')}</Th>
                <Th>{t('common:balance')}</Th>
                <Th>{t('admin:usersMultiplier')}</Th>
                <Th>{t('common:actions')}</Th>
              </Tr>
            </THead>
            <TBody>
              {(users.data?.data ?? []).map((u) => (
                <Tr key={u.id} className={u.id === selected ? 'bg-muted/60' : undefined}>
                  <Td>{u.id}</Td>
                  <Td className="max-w-48 truncate">{u.username}</Td>
                  <Td className="max-w-48 truncate text-xs">{u.email ?? '—'}</Td>
                  <Td>
                    <Badge variant={u.role >= 10 ? 'success' : 'muted'}>
                      {roleLabel(u.role, t)}
                      {u.admin_role_id !== null ? ` #${u.admin_role_id}` : ''}
                    </Badge>
                  </Td>
                  <Td>
                    <Badge variant={u.status === 1 ? 'success' : 'destructive'}>
                      {u.status === 1 ? t('common:enabled') : t('common:disabled')}
                    </Badge>
                  </Td>
                  <Td>{formatMoney(u.balance_micro, i18n.language)}</Td>
                  <Td className="font-mono text-xs">×{u.price_multiplier}</Td>
                  <Td>
                    <Button size="sm" variant="outline" onClick={() => onSelect(u.id)}>
                      {t('admin:usersManage')}
                    </Button>
                  </Td>
                </Tr>
              ))}
            </TBody>
          </Table>
        )}
      </CardContent>
    </Card>
  )
}

function UserActionsCard({ userId }: { userId: number }) {
  const { t, i18n } = useTranslation()
  const queryClient = useQueryClient()
  const [amount, setAmount] = useState('')
  const [reason, setReason] = useState('')
  const [role, setRole] = useState('')
  const [adminRoleId, setAdminRoleId] = useState('')
  const [msg, setMsg] = useState<string | null>(null)

  const overview = useQuery({
    queryKey: qk.userOverview(userId),
    queryFn: () => apiFetch<Overview>(`/admin/users/${userId}/overview`),
  })
  const roles = useQuery({
    queryKey: qk.adminRoles,
    queryFn: () => apiFetch<{ data: RoleRow[] }>('/admin/roles'),
  })

  const invalidate = () => {
    void queryClient.invalidateQueries({ queryKey: qk.userOverview(userId) })
    void queryClient.invalidateQueries({ queryKey: ['admin', 'users'] })
  }

  const credit = useMutation({
    mutationFn: () =>
      apiFetch<{ balance_after_micro: number }>(`/admin/users/${userId}/credit`, {
        method: 'POST',
        // 界面按 USD 填，提交前换成 micro（后端一律 micro-USD 整数）
        body: {
          amount_micro: Math.round((Number(amount) || 0) * 1_000_000),
          reason,
        },
      }),
    onSuccess: (r) => {
      setMsg(t('admin:balanceAfter') + ' ' + formatMoney(r.balance_after_micro, i18n.language))
      setAmount('')
      invalidate()
    },
    onError: (err) => setMsg(describeError(err)),
  })

  const assign = useMutation({
    mutationFn: () =>
      apiFetch(`/admin/users/${userId}/role`, {
        method: 'POST',
        body: {
          role: role === '' ? undefined : Number(role),
          admin_role_id: adminRoleId === '' ? undefined : Number(adminRoleId),
        },
      }),
    onSuccess: () => {
      setMsg(t('common:success'))
      invalidate()
    },
    onError: (err) => setMsg(describeError(err)),
  })

  // 统一管理动作端点：封禁/删除会连带吊销该用户令牌并刷新鉴权缓存
  const manage = useMutation({
    mutationFn: (action: 'ban' | 'unban' | 'promote' | 'demote' | 'delete') =>
      apiFetch(`/admin/users/${userId}/manage`, { method: 'POST', body: { action } }),
    onSuccess: () => {
      setMsg(t('common:success'))
      invalidate()
    },
    onError: (err) => setMsg(describeError(err)),
  })

  const ov = overview.data
  const banned = ov?.user.status === 2

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('admin:usersSelected', { id: userId })}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-4">
        {overview.isError ? (
          <p className="text-sm text-destructive">{describeError(overview.error)}</p>
        ) : (
          <div className="flex flex-wrap gap-2">
            <Badge variant="muted">
              {t('admin:username')} {ov?.user.username ?? '—'}
            </Badge>
            <Badge variant="muted">
              {t('common:balance')} {formatMoney(ov?.user.balance_micro ?? 0, i18n.language)}
            </Badge>
            <Badge variant="muted">
              {t('admin:usersRole')} {roleLabel(ov?.user.role ?? 1, t)}
            </Badge>
            {(ov?.groups ?? []).map((g) => (
              <Badge key={g.code} variant="muted">
                {t('portal:group')} {g.code}
              </Badge>
            ))}
            <Badge variant="muted">
              {t('portal:keys')} {(ov?.keys ?? []).length}
            </Badge>
            <Badge variant={banned ? 'muted' : 'success'}>
              {banned ? t('common:disabled') : t('common:enabled')}
            </Badge>
          </div>
        )}

        <div className="flex flex-wrap items-center gap-2 border-t border-border pt-3">
          <span className="text-sm">{t('admin:userActions')}</span>
          <Button
            size="sm"
            variant="outline"
            onClick={() => manage.mutate(banned ? 'unban' : 'ban')}
          >
            {banned ? t('admin:unban') : t('admin:ban')}
          </Button>
          <Button size="sm" variant="outline" onClick={() => manage.mutate('promote')}>
            {t('admin:promote')}
          </Button>
          <Button size="sm" variant="outline" onClick={() => manage.mutate('demote')}>
            {t('admin:demote')}
          </Button>
          <Button size="sm" variant="destructive" onClick={() => manage.mutate('delete')}>
            {t('admin:softDelete')}
          </Button>
          <span className="text-xs text-muted-foreground">{t('admin:banHint')}</span>
        </div>

        <div className="flex flex-wrap items-end gap-3 border-t border-border pt-3">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="amt">{t('admin:creditUsd')}</Label>
            <Input id="amt" className="w-32" value={amount} onChange={(e) => setAmount(e.target.value)} />
          </div>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="reason">{t('admin:creditReason')}</Label>
            <Input id="reason" value={reason} onChange={(e) => setReason(e.target.value)} />
          </div>
          <Button disabled={credit.isPending || amount.trim() === ''} onClick={() => credit.mutate()}>
            {t('admin:credit')}
          </Button>
        </div>

        <div className="flex flex-wrap items-end gap-3 border-t border-border pt-3">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="role">{t('admin:usersRole')}</Label>
            <select
              id="role"
              className="h-9 rounded-md border border-input bg-card px-2 text-sm"
              value={role}
              onChange={(e) => setRole(e.target.value)}
            >
              <option value="">{t('admin:roleKeep')}</option>
              {BUILTIN_ROLES.map((r) => (
                <option key={r} value={r}>
                  {roleLabel(r, t)}
                </option>
              ))}
            </select>
          </div>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="arole">{t('admin:roleCustom')}</Label>
            <select
              id="arole"
              className="h-9 rounded-md border border-input bg-card px-2 text-sm"
              value={adminRoleId}
              onChange={(e) => setAdminRoleId(e.target.value)}
            >
              <option value="">{t('admin:roleKeep')}</option>
              {(roles.data?.data ?? []).map((r) => (
                <option key={r.id} value={r.id}>
                  {r.display_name} ({r.role_code})
                </option>
              ))}
            </select>
          </div>
          <Button
            variant="outline"
            disabled={assign.isPending || (role === '' && adminRoleId === '')}
            onClick={() => assign.mutate()}
          >
            {t('admin:roleAssign')}
          </Button>
          {msg !== null && <span className="text-xs text-muted-foreground">{msg}</span>}
        </div>
        <p className="text-xs text-muted-foreground">{t('admin:roleAssignHint')}</p>
      </CardContent>
    </Card>
  )
}

function RolesCard() {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const [form, setForm] = useState({ role_code: '', display_name: '' })
  const [picked, setPicked] = useState<Set<string>>(new Set())
  const [msg, setMsg] = useState<string | null>(null)

  const roles = useQuery({
    queryKey: qk.adminRoles,
    queryFn: () => apiFetch<{ data: RoleRow[] }>('/admin/roles'),
  })
  // 权限点清单由后端导出，避免前端硬编码字符串与后端漂移
  const permissions = useQuery({
    queryKey: qk.adminPermissions,
    queryFn: () => apiFetch<{ data: string[] }>('/admin/permissions'),
  })

  const invalidate = () => void queryClient.invalidateQueries({ queryKey: qk.adminRoles })

  const create = useMutation({
    mutationFn: () =>
      apiFetch<{ role_id: number }>('/admin/roles', {
        method: 'POST',
        body: {
          role_code: form.role_code.trim(),
          display_name: form.display_name.trim(),
          permissions: [...picked],
        },
      }),
    onSuccess: () => {
      setMsg(t('common:success'))
      setForm({ role_code: '', display_name: '' })
      setPicked(new Set())
      invalidate()
    },
    onError: (err) => setMsg(describeError(err)),
  })

  const remove = useMutation({
    mutationFn: (code: string) =>
      apiFetch(`/admin/roles/${encodeURIComponent(code)}`, { method: 'DELETE' }),
    onSuccess: () => {
      setMsg(t('common:success'))
      invalidate()
    },
    // 仍有用户绑定时后端回 409 role_in_use，此处直接展示 error_code 的语言包文案
    onError: (err) => setMsg(describeError(err)),
  })

  const togglePerm = (p: string) =>
    setPicked((prev) => {
      const next = new Set(prev)
      if (next.has(p)) next.delete(p)
      else next.add(p)
      return next
    })

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('admin:roleTitle')}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        <p className="text-xs text-muted-foreground">{t('admin:roleHint')}</p>
        <div className="grid grid-cols-2 gap-3 sm:grid-cols-3">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="rcode">{t('admin:roleCode')}</Label>
            <Input
              id="rcode"
              value={form.role_code}
              onChange={(e) => setForm((f) => ({ ...f, role_code: e.target.value }))}
            />
          </div>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="rname">{t('admin:roleName')}</Label>
            <Input
              id="rname"
              value={form.display_name}
              onChange={(e) => setForm((f) => ({ ...f, display_name: e.target.value }))}
            />
          </div>
        </div>
        <div className="flex flex-col gap-1.5">
          <Label>{t('admin:rolePermissions')}</Label>
          <div className="grid grid-cols-2 gap-1.5 sm:grid-cols-3">
            {(permissions.data?.data ?? []).map((p) => (
              <label key={p} className="flex items-center gap-2 font-mono text-xs">
                <input type="checkbox" checked={picked.has(p)} onChange={() => togglePerm(p)} />
                {p}
              </label>
            ))}
          </div>
          {permissions.isError && (
            <span className="text-xs text-destructive">{describeError(permissions.error)}</span>
          )}
        </div>
        <div className="flex items-center gap-3">
          <Button
            disabled={create.isPending || form.role_code.trim() === '' || picked.size === 0}
            onClick={() => create.mutate()}
          >
            {t('common:create')}
          </Button>
          {msg !== null && <span className="text-xs text-muted-foreground">{msg}</span>}
        </div>

        <Table>
          <THead>
            <Tr>
              <Th>ID</Th>
              <Th>{t('admin:roleCode')}</Th>
              <Th>{t('admin:roleName')}</Th>
              <Th>{t('admin:rolePermissions')}</Th>
              <Th>{t('common:actions')}</Th>
            </Tr>
          </THead>
          <TBody>
            {(roles.data?.data ?? []).map((r) => (
              <Tr key={r.id}>
                <Td>{r.id}</Td>
                <Td className="font-mono text-xs">{r.role_code}</Td>
                <Td>{r.display_name}</Td>
                <Td className="max-w-72 truncate font-mono text-xs">
                  {JSON.stringify(r.permissions)}
                </Td>
                <Td>
                  <Button
                    size="sm"
                    variant="destructive"
                    onClick={() => remove.mutate(r.role_code)}
                  >
                    {t('common:delete')}
                  </Button>
                </Td>
              </Tr>
            ))}
          </TBody>
        </Table>
      </CardContent>
    </Card>
  )
}
