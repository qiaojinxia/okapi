import { useQuery } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { useTranslation } from 'react-i18next'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { EmptyState, ErrorState } from '@/components/ui/state'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatCount, formatMoney } from '@/lib/money'

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
                  <Td>
                    {/* 榜首用户消费异常时，下一步永远是"看他都调了什么"——直达明细 */}
                    <Link
                      to="/admin/logs"
                      search={{ user_id: Number(row.user_id), hours: days * 24 }}
                      className="underline decoration-dotted hover:text-foreground"
                    >
                      {String(row.user_id)}
                    </Link>
                  </Td>
                  <Td>{row.username || '—'}</Td>
                  <Td>{formatCount(Number(row.requests), i18n.language)}</Td>
                  <Td>{formatCount(Number(row.tokens), i18n.language)}</Td>
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
