import { useQuery } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { EmptyState, ErrorState } from '@/components/ui/state'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatBp, formatCount } from '@/lib/money'
import { qk } from '@/lib/query-keys'

interface ErrorRow {
  error_code: string
  errors: number
  share_bp: number
  upstream_status: number
  top_channel_id: number
  top_channel_name: string
  top_model: string
}

/// 错误分布卡（老 ok-api error-breakdown 的吸收版，数据源 mv_error_hour）。
///
/// 概览卡的错误率只回答"坏得多不多"；这里回答"坏在哪"——每个错误码带
/// 占比条、最常出错的渠道与模型，点错误码直接跳日志页过滤到这批明细。
export function ErrorBreakdownCard({ days }: { days: number }) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const q = useQuery({
    queryKey: qk.statsErrors(days),
    queryFn: () =>
      apiFetch<{ total: number; data: ErrorRow[] }>(`/admin/stats/errors?days=${days}&limit=20`),
  })

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('admin:statErrors')}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        <p className="text-xs text-muted-foreground">{t('admin:statErrorsHint')}</p>
        {q.isError ? (
          <ErrorState message={describeError(q.error)} />
        ) : (q.data?.data ?? []).length === 0 ? (
          <EmptyState hint={t('admin:statErrorsEmpty')} />
        ) : (
          <Table>
            <THead>
              <Tr>
                <Th>{t('admin:logsErrorCode')}</Th>
                <Th>{t('common:count')}</Th>
                <Th className="w-1/3">{t('admin:statErrorShare')}</Th>
                <Th>{t('admin:logsUpstreamStatus')}</Th>
                <Th>{t('admin:statErrorTopChannel')}</Th>
                <Th>{t('admin:statErrorTopModel')}</Th>
              </Tr>
            </THead>
            <TBody>
              {(q.data?.data ?? []).map((r) => (
                <Tr key={r.error_code}>
                  <Td>
                    {/* 深链到日志页：带上错误码与同一时间窗，落地即是这批失败明细 */}
                    <Link
                      to="/admin/logs"
                      search={
                        r.error_code
                          ? { error_code: r.error_code, hours: days * 24 }
                          : { errors_only: true, hours: days * 24 }
                      }
                      className="font-mono text-xs underline decoration-dotted hover:text-foreground"
                    >
                      {r.error_code || '(empty)'}
                    </Link>
                  </Td>
                  <Td>{formatCount(r.errors, locale)}</Td>
                  <Td>
                    <div className="flex items-center gap-2">
                      <div className="h-2 flex-1 overflow-hidden rounded bg-muted">
                        <div
                          className="h-full bg-destructive/70"
                          style={{ width: `${Math.max(1, r.share_bp / 100)}%` }}
                        />
                      </div>
                      <span className="w-14 shrink-0 text-right text-xs">
                        {formatBp(r.share_bp, locale)}
                      </span>
                    </div>
                  </Td>
                  <Td>
                    {r.upstream_status > 0 ? (
                      <Badge variant="muted">{r.upstream_status}</Badge>
                    ) : (
                      '—'
                    )}
                  </Td>
                  <Td className="max-w-32 truncate text-xs">
                    {r.top_channel_name || (r.top_channel_id > 0 ? `#${r.top_channel_id}` : '—')}
                  </Td>
                  <Td className="font-mono text-xs">{r.top_model || '—'}</Td>
                </Tr>
              ))}
            </TBody>
          </Table>
        )}
      </CardContent>
    </Card>
  )
}
