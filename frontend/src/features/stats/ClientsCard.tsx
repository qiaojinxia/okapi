import { useQuery } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { EmptyState, ErrorState } from '@/components/ui/state'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { healthVariant } from '@/features/stats/ChannelHealthCard'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatBp, formatCount, formatMoney } from '@/lib/money'
import { qk } from '@/lib/query-keys'

interface ClientRow {
  client_type: string
  requests: number
  share_bp: number
  tokens: number
  amount_micro: number
  errors: number
  error_rate_bp: number
  users: number
}

/// 客户端类型分布（#5277，数据源 mv_client_day）。
///
/// 回答"流量从哪些工具来"。占比条按请求数，但另给**去重用户数**一列：
/// 一个脚本刷十万次和一百个人各用一千次，请求占比可以一样，含义完全不同。
export function ClientsCard({ days }: { days: number }) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const q = useQuery({
    queryKey: qk.statsClients(days),
    queryFn: () =>
      apiFetch<{ total_requests: number; data: ClientRow[] }>(
        `/admin/stats/clients?days=${days}&limit=30`,
      ),
  })
  const rows = q.data?.data ?? []

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('admin:statClients')}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        <p className="text-xs text-muted-foreground">{t('admin:statClientsHint')}</p>
        {q.isError ? (
          <ErrorState message={describeError(q.error)} />
        ) : rows.length === 0 ? (
          <EmptyState hint={t('admin:trendEmptyHint')} />
        ) : (
          <Table>
            <THead>
              <Tr>
                <Th>{t('admin:logsClient')}</Th>
                <Th className="w-1/3">{t('admin:statErrorShare')}</Th>
                <Th>{t('common:requests')}</Th>
                <Th>{t('admin:statClientUsers')}</Th>
                <Th>{t('common:tokens')}</Th>
                <Th>{t('admin:statErrorRate')}</Th>
                <Th>{t('common:amount')}</Th>
              </Tr>
            </THead>
            <TBody>
              {rows.map((r) => (
                <Tr key={r.client_type || '__unknown'}>
                  <Td className="font-mono text-xs">
                    {r.client_type || (
                      <span className="font-sans text-muted-foreground">
                        {t('admin:statClientUnknown')}
                      </span>
                    )}
                  </Td>
                  <Td>
                    <div className="flex items-center gap-2">
                      <div className="h-2 flex-1 overflow-hidden rounded bg-muted">
                        <div
                          className="h-full bg-primary/70"
                          style={{ width: `${Math.max(1, r.share_bp / 100)}%` }}
                        />
                      </div>
                      <span className="w-14 shrink-0 text-right text-xs">
                        {formatBp(r.share_bp, locale)}
                      </span>
                    </div>
                  </Td>
                  <Td>{formatCount(r.requests, locale)}</Td>
                  <Td>{formatCount(r.users, locale)}</Td>
                  <Td>{formatCount(r.tokens, locale)}</Td>
                  <Td>
                    <Badge variant={healthVariant(r.error_rate_bp)}>
                      {formatBp(r.error_rate_bp, locale)}
                    </Badge>
                  </Td>
                  <Td>{formatMoney(r.amount_micro, locale)}</Td>
                </Tr>
              ))}
            </TBody>
          </Table>
        )}
      </CardContent>
    </Card>
  )
}
