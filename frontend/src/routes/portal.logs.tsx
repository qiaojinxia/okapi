import { useQuery } from '@tanstack/react-query'
import { createFileRoute } from '@tanstack/react-router'
import dayjs from 'dayjs'
import { Fragment, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatMoney } from '@/lib/money'
import { qk } from '@/lib/query-keys'

export const Route = createFileRoute('/portal/logs')({
  component: LogsPage,
})

interface AppliedRule {
  code: string
  kind: string
  multiplier: string
}

interface Snapshot {
  mode: string
  model_ratio: string | null
  completion_ratio: string | null
  cache_ratio: string | null
  group: string
  group_ratio: string
  user_multiplier: string
  rules: AppliedRule[]
}

interface LogRow {
  id: number
  request_id: string
  model: string
  log_type: number
  status: number
  usage: {
    prompt_tokens: number
    cached_tokens: number
    completion_tokens: number
    reasoning_tokens: number
  }
  amount_micro: number
  original_amount_micro: number
  discount_micro: number
  pricing_snapshot: Snapshot | null
  error_code: string | null
  latency_ms: number | null
  is_stream: boolean
  created_at: string
}

function LogsPage() {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const [expanded, setExpanded] = useState<number | null>(null)

  const logs = useQuery({
    queryKey: qk.logs,
    queryFn: () => apiFetch<{ data: LogRow[] }>('/api/me/logs?limit=100'),
  })

  if (logs.isError) {
    return <p className="text-sm text-destructive">{describeError(logs.error)}</p>
  }
  const rows = logs.data?.data ?? []
  return (
    <div className="flex flex-col gap-3">
      <div className="flex items-center justify-between">
        <h2 className="text-sm font-semibold text-muted-foreground">{t('logs:title')}</h2>
        <Button size="sm" variant="outline" onClick={() => void logs.refetch()}>
          {t('common:refresh')}
        </Button>
      </div>
      <Table>
        <THead>
          <Tr>
            <Th>{t('logs:time')}</Th>
            <Th>{t('common:status')}</Th>
            <Th>{t('pricing:model')}</Th>
            <Th>{t('logs:tokens')}</Th>
            <Th>{t('common:amount')}</Th>
            <Th>{t('logs:latency')}</Th>
          </Tr>
        </THead>
        <TBody>
          {rows.map((r) => (
            <Fragment key={r.id}>
              <Tr
                className="cursor-pointer"
                onClick={() => setExpanded(expanded === r.id ? null : r.id)}
              >
                <Td className="whitespace-nowrap text-xs">
                  {dayjs(r.created_at).format('MM-DD HH:mm:ss')}
                </Td>
                <Td>
                  <Badge variant={r.status === 20 ? 'success' : 'destructive'}>
                    {r.status === 20 ? t('logs:ok') : (r.error_code ?? t('logs:failed'))}
                  </Badge>
                </Td>
                <Td className="font-mono text-xs">{r.model}</Td>
                <Td className="text-xs">
                  {r.usage.prompt_tokens}
                  {r.usage.cached_tokens > 0 && (
                    <span className="text-muted-foreground">
                      ({t('logs:cachedShort', { n: r.usage.cached_tokens })})
                    </span>
                  )}
                  {' + '}
                  {r.usage.completion_tokens}
                </Td>
                <Td>{formatMoney(r.amount_micro, locale)}</Td>
                <Td className="text-xs">{r.latency_ms === null ? '—' : `${r.latency_ms}ms`}</Td>
              </Tr>
              {expanded === r.id && (
                <Tr>
                  <Td colSpan={6} className="bg-muted/30">
                    <BillExplainer row={r} locale={locale} />
                  </Td>
                </Tr>
              )}
            </Fragment>
          ))}
        </TBody>
      </Table>
      {rows.length === 0 && <p className="text-sm text-muted-foreground">{t('common:empty')}</p>}
    </div>
  )
}

/// 账单解释器：吃 pricing_snapshot 逐层展开（DESIGN §3：snapshot 是计费唯一语义）。
function BillExplainer({ row, locale }: { row: LogRow; locale: string }) {
  const { t } = useTranslation()
  const s = row.pricing_snapshot
  return (
    <div className="flex flex-col gap-2 p-2 text-xs">
      <div className="flex flex-wrap gap-4">
        <span>
          {t('logs:original')}：
          <strong>{formatMoney(row.original_amount_micro, locale)}</strong>
        </span>
        {row.discount_micro > 0 && (
          <span className="text-success">
            {t('logs:discount')}：-{formatMoney(row.discount_micro, locale)}
          </span>
        )}
        <span>
          {t('logs:final')}：<strong>{formatMoney(row.amount_micro, locale)}</strong>
        </span>
        <span className="font-mono text-muted-foreground">{row.request_id}</span>
      </div>
      {s ? (
        <div className="flex flex-wrap gap-2">
          <Badge variant="muted">
            {t('logs:mode')} {s.mode}
          </Badge>
          {s.model_ratio !== null && <Badge variant="muted">{t('admin:modelRatio')} ×{s.model_ratio}</Badge>}
          {s.completion_ratio !== null && (
            <Badge variant="muted">{t('admin:completionRatio')} ×{s.completion_ratio}</Badge>
          )}
          {s.cache_ratio !== null && row.usage.cached_tokens > 0 && (
            <Badge variant="muted">{t('admin:cacheRatio')} ×{s.cache_ratio}</Badge>
          )}
          <Badge variant="muted">
            {t('logs:group')} {s.group} ×{s.group_ratio}
          </Badge>
          {s.user_multiplier !== '1' && (
            <Badge variant="muted">{t('logs:userMultiplier')} ×{s.user_multiplier}</Badge>
          )}
          {s.rules.map((rule) => (
            <Badge key={rule.code}>
              {rule.code} ×{rule.multiplier}
            </Badge>
          ))}
        </div>
      ) : (
        <p className="text-muted-foreground">{t('logs:noSnapshot')}</p>
      )}
    </div>
  )
}
