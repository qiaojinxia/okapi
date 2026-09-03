import { Settings2, Users } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import type { TeamRow } from '@/features/teams/types'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { IconButton } from '@/components/ui/icon-button'
import { TableSkeleton } from '@/components/ui/skeleton'
import { EmptyState, ErrorState } from '@/components/ui/state'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { formatMoney } from '@/lib/money'

export function TeamListCard({
  teams,
  loading,
  error,
  onPick,
  onCreate,
}: {
  teams: TeamRow[]
  loading: boolean
  error: string | null
  onPick: (id: number) => void
  onCreate: () => void
}) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language

  if (error !== null) return <ErrorState message={error} />
  if (loading) return <TableSkeleton rows={3} cols={6} />
  if (teams.length === 0) {
    return (
      <EmptyState
        icon={Users}
        hint={t('team:emptyHint')}
        action={<Button onClick={onCreate}>{t('team:create')}</Button>}
      />
    )
  }

  return (
    <Table>
      <THead>
        <Tr>
          <Th>ID</Th>
          <Th>{t('team:name')}</Th>
          <Th>{t('team:myRole')}</Th>
          <Th numeric>{t('team:members')}</Th>
          <Th numeric>{t('common:balance')}</Th>
          <Th className="w-16 text-right">{t('common:actions')}</Th>
        </Tr>
      </THead>
      <TBody>
        {teams.map((tm) => (
          <Tr key={tm.team_id} className="cursor-pointer" onClick={() => onPick(tm.team_id)}>
            <Td className="text-xs text-muted-foreground tabular-nums">{tm.team_id}</Td>
            <Td className="font-medium">{tm.name}</Td>
            <Td>
              <Badge variant={tm.role === 'owner' ? 'success' : 'muted'}>{tm.role}</Badge>
            </Td>
            <Td numeric>{tm.member_count}</Td>
            <Td numeric>{formatMoney(tm.balance_micro, locale)}</Td>
            <Td className="text-right">
              <IconButton
                icon={Settings2}
                label={t('team:manage')}
                variant="outline"
                onClick={() => onPick(tm.team_id)}
              />
            </Td>
          </Tr>
        ))}
      </TBody>
    </Table>
  )
}
