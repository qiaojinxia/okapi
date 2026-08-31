import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { createFileRoute } from '@tanstack/react-router'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Input, Label } from '@/components/ui/input'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { ApiError, apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatMoney } from '@/lib/money'
import { qk } from '@/lib/query-keys'

export const Route = createFileRoute('/portal/teams')({
  component: TeamsPage,
})

interface TeamRow {
  team_id: number
  name: string
  role: string
  member_count: number
  monthly_spend_limit_micro: number | null
  balance_micro: number
}

interface MemberRow {
  member_user_id: number
  username: string
  role: string
  monthly_spend_limit_micro: number | null
  total_spend_micro: number
  month_spend_micro: number
}

interface UsageResp {
  team_id: number
  balance_micro: number
  members: MemberRow[]
}

/// Team 层为 web session 鉴权（成员自助），与门户的 API key 单轨不同：
/// 用 API Key 方式登录的浏览器没有 session cookie，会 401。此处统一降级为提示，
/// 引导改用邮箱密码登录，而不是让页面看起来"没有团队"。
function sessionRequired(err: unknown): boolean {
  return err instanceof ApiError && err.status === 401
}

function TeamsPage() {
  const { t } = useTranslation()
  const [active, setActive] = useState<number | null>(null)

  const teams = useQuery({
    queryKey: qk.myTeams,
    queryFn: () => apiFetch<{ data: TeamRow[] }>('/api/teams'),
    retry: false,
  })

  if (teams.isError && sessionRequired(teams.error)) {
    return (
      <Card>
        <CardHeader>
          <CardTitle>{t('team:title')}</CardTitle>
        </CardHeader>
        <CardContent className="text-sm text-muted-foreground">
          {t('team:sessionRequired')}
        </CardContent>
      </Card>
    )
  }

  return (
    <div className="flex flex-col gap-4">
      <TeamListCard
        teams={teams.data?.data ?? []}
        error={teams.isError ? describeError(teams.error) : null}
        active={active}
        onPick={setActive}
      />
      {active !== null && <TeamDetailCard teamId={active} />}
    </div>
  )
}

function TeamListCard({
  teams,
  error,
  active,
  onPick,
}: {
  teams: TeamRow[]
  error: string | null
  active: number | null
  onPick: (id: number) => void
}) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const queryClient = useQueryClient()
  const [name, setName] = useState('')
  const [msg, setMsg] = useState<string | null>(null)

  const create = useMutation({
    mutationFn: () =>
      apiFetch<{ team_id: number }>('/api/teams', {
        method: 'POST',
        body: { name: name.trim() },
      }),
    onSuccess: () => {
      setMsg(t('common:success'))
      setName('')
      void queryClient.invalidateQueries({ queryKey: qk.myTeams })
    },
    onError: (err) => setMsg(describeError(err)),
  })

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('team:title')}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        <p className="text-xs text-muted-foreground">{t('team:hint')}</p>
        <div className="flex flex-wrap items-end gap-3">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="tname">{t('team:name')}</Label>
            <Input id="tname" value={name} onChange={(e) => setName(e.target.value)} />
          </div>
          <Button disabled={create.isPending || name.trim() === ''} onClick={() => create.mutate()}>
            {t('team:create')}
          </Button>
          {msg !== null && <span className="text-xs text-muted-foreground">{msg}</span>}
        </div>

        {error !== null && <p className="text-sm text-destructive">{error}</p>}
        {teams.length === 0 ? (
          <p className="text-sm text-muted-foreground">{t('common:empty')}</p>
        ) : (
          <Table>
            <THead>
              <Tr>
                <Th>ID</Th>
                <Th>{t('team:name')}</Th>
                <Th>{t('team:myRole')}</Th>
                <Th>{t('team:members')}</Th>
                <Th>{t('common:balance')}</Th>
                <Th>{t('common:actions')}</Th>
              </Tr>
            </THead>
            <TBody>
              {teams.map((tm) => (
                <Tr key={tm.team_id}>
                  <Td>{tm.team_id}</Td>
                  <Td>{tm.name}</Td>
                  <Td>
                    <Badge variant={tm.role === 'owner' ? 'success' : 'muted'}>{tm.role}</Badge>
                  </Td>
                  <Td>{tm.member_count}</Td>
                  <Td>{formatMoney(tm.balance_micro, locale)}</Td>
                  <Td>
                    <Button
                      size="sm"
                      variant={active === tm.team_id ? 'default' : 'outline'}
                      onClick={() => onPick(tm.team_id)}
                    >
                      {t('team:manage')}
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

function TeamDetailCard({ teamId }: { teamId: number }) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const queryClient = useQueryClient()
  const [msg, setMsg] = useState<string | null>(null)
  const [issued, setIssued] = useState<string | null>(null)
  const [form, setForm] = useState({ user_id: '', role: 'member', limit: '' })

  const usage = useQuery({
    queryKey: qk.teamUsage(teamId),
    queryFn: () => apiFetch<UsageResp>(`/api/teams/${teamId}/usage`),
    retry: false,
  })
  const invalidate = () => void queryClient.invalidateQueries({ queryKey: qk.teamUsage(teamId) })

  const upsertMember = useMutation({
    mutationFn: () =>
      apiFetch(`/api/teams/${teamId}/members`, {
        method: 'POST',
        body: {
          user_id: Number(form.user_id),
          role: form.role,
          // 界面按 USD 填，提交转 micro（后端一律 micro-USD 整数）
          monthly_spend_limit_micro:
            form.limit.trim() === '' ? null : Math.round(Number(form.limit) * 1_000_000),
        },
      }),
    onSuccess: () => {
      setMsg(t('common:success'))
      setForm({ user_id: '', role: 'member', limit: '' })
      invalidate()
    },
    onError: (err) => setMsg(describeError(err)),
  })

  const issueKey = useMutation({
    mutationFn: () =>
      apiFetch<{ api_key: string }>(`/api/teams/${teamId}/keys`, {
        method: 'POST',
        body: { name: 'team' },
      }),
    // 明文仅本次返回，故直接呈现并提示不可再取
    onSuccess: (r) => setIssued(r.api_key),
    onError: (err) => setMsg(describeError(err)),
  })

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('team:detail', { id: teamId })}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        <div className="flex flex-wrap items-center gap-3">
          <Badge variant="muted">
            {t('common:balance')} {formatMoney(usage.data?.balance_micro ?? 0, locale)}
          </Badge>
          <Button size="sm" variant="outline" onClick={() => issueKey.mutate()}>
            {t('team:issueKey')}
          </Button>
          {msg !== null && <span className="text-xs text-muted-foreground">{msg}</span>}
        </div>

        {issued !== null && (
          <div className="flex flex-col gap-1 rounded-md border border-border p-2">
            <span className="font-mono text-xs break-all">{issued}</span>
            <span className="text-xs text-destructive">{t('team:keyOnceOnly')}</span>
          </div>
        )}

        <div className="flex flex-wrap items-end gap-3 border-t border-border pt-3">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="muid">{t('team:memberUserId')}</Label>
            <Input
              id="muid"
              className="w-28"
              inputMode="numeric"
              value={form.user_id}
              onChange={(e) => setForm((f) => ({ ...f, user_id: e.target.value }))}
            />
          </div>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="mrole">{t('team:memberRole')}</Label>
            <select
              id="mrole"
              className="h-9 rounded-md border border-input bg-card px-2 text-sm"
              value={form.role}
              onChange={(e) => setForm((f) => ({ ...f, role: e.target.value }))}
            >
              <option value="member">member</option>
              <option value="admin">admin</option>
            </select>
          </div>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="mlimit">{t('team:monthlyLimit')}</Label>
            <Input
              id="mlimit"
              className="w-32"
              value={form.limit}
              placeholder={t('team:noLimit')}
              onChange={(e) => setForm((f) => ({ ...f, limit: e.target.value }))}
            />
          </div>
          <Button
            size="sm"
            disabled={upsertMember.isPending || form.user_id.trim() === ''}
            onClick={() => upsertMember.mutate()}
          >
            {t('team:upsertMember')}
          </Button>
        </div>

        {usage.isError ? (
          <p className="text-sm text-destructive">{describeError(usage.error)}</p>
        ) : (
          <Table>
            <THead>
              <Tr>
                <Th>{t('admin:username')}</Th>
                <Th>{t('team:memberRole')}</Th>
                <Th>{t('team:monthSpend')}</Th>
                <Th>{t('team:monthlyLimit')}</Th>
                <Th>{t('team:totalSpend')}</Th>
              </Tr>
            </THead>
            <TBody>
              {(usage.data?.members ?? []).map((m) => (
                <Tr key={m.member_user_id}>
                  <Td>{m.username}</Td>
                  <Td>
                    <Badge variant={m.role === 'owner' ? 'success' : 'muted'}>{m.role}</Badge>
                  </Td>
                  <Td>{formatMoney(m.month_spend_micro, locale)}</Td>
                  <Td>
                    {m.monthly_spend_limit_micro === null
                      ? t('team:noLimit')
                      : formatMoney(m.monthly_spend_limit_micro, locale)}
                  </Td>
                  <Td>{formatMoney(m.total_spend_micro, locale)}</Td>
                </Tr>
              ))}
            </TBody>
          </Table>
        )}
      </CardContent>
    </Card>
  )
}
