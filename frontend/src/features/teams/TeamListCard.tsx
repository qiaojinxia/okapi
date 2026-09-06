import { Settings2, Users } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { teamRoleLabel, type TeamRow } from '@/features/teams/types'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { IconButton } from '@/components/ui/icon-button'
import { Pagination } from '@/components/ui/pagination'
import { TableSkeleton } from '@/components/ui/skeleton'
import { EmptyState, ErrorState } from '@/components/ui/state'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import type { Pager } from '@/hooks/use-pagination'
import { formatMoney } from '@/lib/money'

/// 团队列表（一页）。分页状态由页面持有：列表接口按 `pager` 取页，这里只画当前页与翻页器。
export function TeamListCard({
  teams,
  total,
  pager,
  loading,
  error,
  onPick,
  onCreate,
}: {
  teams: TeamRow[]
  total: number | undefined
  pager: Pager
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
    <div className="flex flex-col gap-3">
      <Table stickyHeader>
        <THead>
          <Tr>
            {/* w-0 + nowrap 让短列吃内容宽；团队名 w-full 吃掉剩余宽度，避免角色/人数列被撑出大空洞 */}
            <Th className="w-0">ID</Th>
            <Th className="w-full">{t('team:name')}</Th>
            <Th className="w-0">{t('team:myRole')}</Th>
            <Th numeric className="w-0">{t('team:members')}</Th>
            <Th numeric className="w-0">{t('common:balance')}</Th>
            <Th className="w-0 text-right">{t('common:actions')}</Th>
          </Tr>
        </THead>
        <TBody>
          {teams.map((tm) => (
            <Tr key={tm.team_id} className="cursor-pointer" onClick={() => onPick(tm.team_id)}>
              <Td className="text-xs text-muted-foreground tabular-nums">{tm.team_id}</Td>
              <Td className="font-medium">{tm.name}</Td>
              <Td>
                <Badge variant={tm.role === 'owner' ? 'success' : 'muted'}>
                  {teamRoleLabel(tm.role, t)}
                </Badge>
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
      <Pagination {...pager} total={total} />
    </div>
  )
}
