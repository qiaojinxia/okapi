import { useQuery } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { useTranslation } from 'react-i18next'
import type { ChannelRow } from '@/features/stats/types'
import { BAD_BP, WARN_BP } from '@/features/stats/types'
import { Badge } from '@/components/ui/badge'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Pagination } from '@/components/ui/pagination'
import { EmptyState, LoadingState } from '@/components/ui/state'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatBp, formatCount, formatMoney, formatTokensPerSec } from '@/lib/money'
import { qk } from '@/lib/query-keys'
import { usePagination } from '@/hooks/use-pagination'

export function healthVariant(bp: number): 'success' | 'muted' | 'destructive' {
  if (bp >= BAD_BP) return 'destructive'
  if (bp >= WARN_BP) return 'muted'
  return 'success'
}


export function ChannelHealthCard({ days }: { days: number }) {
  const { t, i18n } = useTranslation()
  const pager = usePagination()
  const q = useQuery({
    queryKey: qk.statsChannels(days, pager.limit, pager.offset),
    queryFn: () => apiFetch<{ total_items: number; data: ChannelRow[] }>(`/admin/stats/channels?days=${days}&limit=${pager.limit}&offset=${pager.offset}`),
  })

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('admin:statChannels')}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        <p className="text-xs text-muted-foreground">{t('admin:statChannelsHint')}</p>
        {q.isError ? (
          <p className="text-sm text-destructive">{describeError(q.error)}</p>
        ) : q.isPending ? <LoadingState /> : q.data.data.length === 0 ? <EmptyState hint={t('admin:trendEmptyHint')} /> : (
          <Table>
            <THead>
              <Tr>
                <Th>{t('admin:channelName')}</Th>
                <Th>{t('admin:provider')}</Th>
                <Th>{t('common:requests')}</Th>
                <Th>{t('admin:statErrorRate')}</Th>
                <Th>TTFT p50 / p95 / p99</Th>
                <Th>{t('admin:statTps')}</Th>
                <Th>{t('admin:statStickyRate')}</Th>
                <Th>{t('admin:statFailovers')}</Th>
                <Th>{t('common:amount')}</Th>
              </Tr>
            </THead>
            <TBody>
              {(q.data?.data ?? []).map((c) => (
                <Tr key={c.channel_id}>
                  <Td className="max-w-48 truncate">
                    {/* 红灯渠道 → 一键到该渠道的失败明细，不必记 id 再去日志页手敲 */}
                    <Link
                      to="/admin/logs"
                      search={{
                        channel_id: c.channel_id,
                        hours: days * 24,
                        errors_only: c.error_rate_bp >= WARN_BP ? true : undefined,
                      }}
                      className="underline decoration-dotted hover:text-foreground"
                    >
                      {c.name || `#${c.channel_id}`}
                    </Link>
                  </Td>
                  <Td>
                    <Badge variant="muted">{c.provider}</Badge>
                  </Td>
                  <Td>{formatCount(c.requests, i18n.language)}</Td>
                  <Td>
                    <Badge variant={healthVariant(c.error_rate_bp)}>
                      {formatBp(c.error_rate_bp, i18n.language)}
                    </Badge>
                  </Td>
                  <Td className="font-mono text-xs">
                    {c.ttft_p50_ms} / {c.ttft_p95_ms} / {c.ttft_p99_ms} ms
                  </Td>
                  <Td>{formatTokensPerSec(c.tokens_per_1k_sec, i18n.language)}</Td>
                  <Td>{formatBp(c.sticky_rate_bp, i18n.language)}</Td>
                  <Td>{formatCount(c.failovers, i18n.language)}</Td>
                  <Td>{formatMoney(c.amount_micro, i18n.language)}</Td>
                </Tr>
              ))}
            </TBody>
          </Table>
        )}
        <Pagination {...pager} total={q.isError ? undefined : q.data?.total_items} disabled={q.isFetching} className="rounded-lg shadow-none" />
      </CardContent>
    </Card>
  )
}
