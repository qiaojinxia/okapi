import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { KeyRound, UserPlus, Wallet } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { UsageResp } from '@/features/teams/types'
import { Alert } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { CopyButton } from '@/components/ui/copy-button'
import { FieldGroup } from '@/components/ui/drawer'
import { Field } from '@/components/ui/field'
import { Input } from '@/components/ui/input'
import { Select } from '@/components/ui/select'
import { TableSkeleton } from '@/components/ui/skeleton'
import { ErrorState } from '@/components/ui/state'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { toast } from '@/components/ui/toast'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatMoney } from '@/lib/money'
import { qk } from '@/lib/query-keys'

/// 团队详情（抽屉内容）：钱包与发 key 在顶部，成员表居中，加成员表单在底部分区。
export function TeamDetailCard({ teamId }: { teamId: number }) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const queryClient = useQueryClient()
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
      toast.success(t('common:success'))
      setForm({ user_id: '', role: 'member', limit: '' })
      invalidate()
    },
    onError: (err) => toast.error(describeError(err)),
  })

  const issueKey = useMutation({
    mutationFn: () =>
      apiFetch<{ api_key: string }>(`/api/teams/${teamId}/keys`, {
        method: 'POST',
        body: { name: 'team' },
      }),
    // 明文仅本次返回，故直接呈现并提示不可再取
    onSuccess: (r) => setIssued(r.api_key),
    onError: (err) => toast.error(describeError(err)),
  })

  return (
    <div className="flex flex-col">
      <FieldGroup title={t('common:balance')}>
        <div className="flex flex-wrap items-center justify-between gap-3 rounded-lg border border-border bg-muted/30 px-4 py-3">
          <div className="flex items-center gap-3">
            <span className="flex h-9 w-9 items-center justify-center rounded-md bg-primary/10 text-primary">
              <Wallet className="h-4 w-4" />
            </span>
            <div className="flex flex-col">
              <span className="text-xs text-muted-foreground">{t('common:balance')}</span>
              <span className="text-lg font-semibold tabular-nums">
                {formatMoney(usage.data?.balance_micro ?? 0, locale)}
              </span>
            </div>
          </div>
          <Button size="sm" variant="outline" loading={issueKey.isPending} onClick={() => issueKey.mutate()}>
            <KeyRound className="h-4 w-4" />
            {t('team:issueKey')}
          </Button>
        </div>
        {issued !== null && (
          <Alert tone="warning" title={t('team:keyOnceOnly')} onClose={() => setIssued(null)}>
            <div className="mt-1 flex items-center gap-2 rounded-md border border-border bg-card p-2">
              <code className="min-w-0 flex-1 font-mono text-xs break-all text-foreground">{issued}</code>
              <CopyButton value={issued} />
            </div>
          </Alert>
        )}
      </FieldGroup>

      <FieldGroup title={t('team:members')}>
        {usage.isError ? (
          <ErrorState message={describeError(usage.error)} />
        ) : usage.isPending ? (
          <TableSkeleton rows={3} cols={5} />
        ) : (
          <Table dense>
            <THead>
              <Tr>
                <Th>{t('admin:username')}</Th>
                <Th>{t('team:memberRole')}</Th>
                <Th numeric>{t('team:monthSpend')}</Th>
                <Th numeric>{t('team:monthlyLimit')}</Th>
                <Th numeric>{t('team:totalSpend')}</Th>
              </Tr>
            </THead>
            <TBody>
              {usage.data.members.map((m) => (
                <Tr key={m.member_user_id}>
                  <Td className="font-medium">{m.username}</Td>
                  <Td>
                    <Badge variant={m.role === 'owner' ? 'success' : 'muted'}>{m.role}</Badge>
                  </Td>
                  <Td numeric>{formatMoney(m.month_spend_micro, locale)}</Td>
                  <Td numeric className="text-muted-foreground">
                    {m.monthly_spend_limit_micro === null
                      ? t('team:noLimit')
                      : formatMoney(m.monthly_spend_limit_micro, locale)}
                  </Td>
                  <Td numeric>{formatMoney(m.total_spend_micro, locale)}</Td>
                </Tr>
              ))}
            </TBody>
          </Table>
        )}
      </FieldGroup>

      <FieldGroup title={t('team:upsertMember')} hint={t('team:upsertMemberHint')}>
        <form
          className="grid gap-3 sm:grid-cols-[minmax(0,1fr)_minmax(0,1fr)_minmax(0,1fr)_auto] sm:items-end"
          onSubmit={(e) => {
            e.preventDefault()
            if (form.user_id.trim() !== '') upsertMember.mutate()
          }}
        >
          <Field label={t('team:memberUserId')} htmlFor="muid">
            <Input
              id="muid"
              inputMode="numeric"
              value={form.user_id}
              onChange={(e) => setForm((f) => ({ ...f, user_id: e.target.value }))}
            />
          </Field>
          <Field label={t('team:memberRole')} htmlFor="mrole">
            <Select
              id="mrole"
              value={form.role}
              onChange={(v) => setForm((f) => ({ ...f, role: v }))}
              options={[
                { value: 'member', label: 'member' },
                { value: 'admin', label: 'admin' },
              ]}
            />
          </Field>
          <Field label={t('team:monthlyLimit')} htmlFor="mlimit">
            <Input
              id="mlimit"
              inputMode="decimal"
              value={form.limit}
              placeholder={t('team:noLimit')}
              onChange={(e) => setForm((f) => ({ ...f, limit: e.target.value }))}
            />
          </Field>
          <Button type="submit" loading={upsertMember.isPending} disabled={form.user_id.trim() === ''}>
            <UserPlus className="h-4 w-4" />
            {t('team:upsertMember')}
          </Button>
        </form>
      </FieldGroup>
    </div>
  )
}
