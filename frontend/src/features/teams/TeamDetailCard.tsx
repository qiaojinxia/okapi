import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { UsageResp } from '@/features/teams/types'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Input, Label } from '@/components/ui/input'
import { Select } from '@/components/ui/select'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatMoney } from '@/lib/money'
import { qk } from '@/lib/query-keys'

export function TeamDetailCard({ teamId }: { teamId: number }) {
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
            <Select
              id="mrole"
              className="w-32"
              value={form.role}
              onChange={(v) => setForm((f) => ({ ...f, role: v }))}
              options={[
                { value: 'member', label: 'member' },
                { value: 'admin', label: 'admin' },
              ]}
            />
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
