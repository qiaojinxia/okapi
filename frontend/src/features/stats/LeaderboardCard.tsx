import { useQuery } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { EmptyState, ErrorState } from '@/components/ui/state'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatMoney } from '@/lib/money'

export interface LeaderboardRow {
  user_id: number | string
  username: string
  requests: number | string
  tokens: number | string
  amount_micro: number | string
}


/// 用户消耗排行（#1790-11）：CH mv_user_day 聚合。
export function LeaderboardCard({ days }: { days: number }) {
  const { t, i18n } = useTranslation()
  const board = useQuery({
    queryKey: ['admin-leaderboard', days],
    queryFn: () =>
      apiFetch<{ data: LeaderboardRow[] }>(`/admin/leaderboard?days=${days}&limit=20`),
  })

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('admin:leaderboard')}</CardTitle>
      </CardHeader>
      <CardContent>
        {board.isError ? (
          <ErrorState message={describeError(board.error)} />
        ) : (board.data?.data ?? []).length === 0 ? (
          <EmptyState />
        ) : (
          <Table>
            <THead>
              <Tr>
                <Th>#</Th>
                <Th>{t('admin:userId')}</Th>
                <Th>{t('admin:username')}</Th>
                <Th>{t('admin:requests')}</Th>
                <Th>{t('admin:tokens')}</Th>
                <Th>{t('admin:spend')}</Th>
              </Tr>
            </THead>
            <TBody>
              {(board.data?.data ?? []).map((row, i) => (
                <Tr key={String(row.user_id)}>
                  <Td className="text-muted-foreground">{i + 1}</Td>
                  <Td>{String(row.user_id)}</Td>
                  <Td>{row.username || '—'}</Td>
                  <Td>{String(row.requests)}</Td>
                  <Td>{String(row.tokens)}</Td>
                  <Td>{formatMoney(Number(row.amount_micro), i18n.language)}</Td>
                </Tr>
              ))}
            </TBody>
          </Table>
        )}
      </CardContent>
    </Card>
  )
}
