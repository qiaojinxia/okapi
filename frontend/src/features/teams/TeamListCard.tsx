import { useMutation, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { TeamRow } from '@/features/teams/types'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Input, Label } from '@/components/ui/input'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatMoney } from '@/lib/money'
import { qk } from '@/lib/query-keys'

export function TeamListCard({
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
