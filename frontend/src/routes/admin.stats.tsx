import { useQuery } from '@tanstack/react-query'
import { createFileRoute } from '@tanstack/react-router'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Bar, BarChart, CartesianGrid, ResponsiveContainer, Tooltip, XAxis, YAxis } from 'recharts'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatBp, formatCount, formatMoney, formatTokensPerSec } from '@/lib/money'
import { qk } from '@/lib/query-keys'

export const Route = createFileRoute('/admin/stats')({
  component: StatsPage,
})

/// 错误率红绿灯阈值（基点）：<1% 绿、<5% 黄、其余红。
const WARN_BP = 100
const BAD_BP = 500

interface ChannelRow {
  channel_id: number
  name: string
  provider: string
  requests: number
  errors: number
  error_rate_bp: number
  ttft_p50_ms: number
  ttft_p95_ms: number
  ttft_p99_ms: number
  failovers: number
  sticky_rate_bp: number
  tokens_per_1k_sec: number
  amount_micro: number
}

interface ModelRow {
  model: string
  requests: number
  tokens: number
  amount_micro: number
  ttft_p50_ms: number
  ttft_p95_ms: number
  ttft_p99_ms: number
  latency_p50_ms: number
  latency_p95_ms: number
  latency_p99_ms: number
  tokens_per_1k_sec: number
}

interface MarginDay {
  day: string
  requests: number
  amount_micro: number
  original_micro: number
  discount_micro: number
}

interface MarginResp {
  data: MarginDay[]
  total: {
    requests: number
    errors: number
    error_rate_bp: number
    amount_micro: number
    discount_micro: number
    upstream_cost_micro: number
    margin_micro: number
  }
}

function DaysPicker({ days, onPick }: { days: number; onPick: (d: number) => void }) {
  const { t } = useTranslation()
  return (
    <div className="flex gap-2">
      {[1, 7, 30].map((d) => (
        <Button
          key={d}
          size="sm"
          variant={days === d ? 'default' : 'outline'}
          onClick={() => onPick(d)}
        >
          {t('admin:lastDays', { days: d })}
        </Button>
      ))}
    </div>
  )
}

function StatsPage() {
  const [days, setDays] = useState(7)
  return (
    <div className="flex flex-col gap-4">
      <DaysPicker days={days} onPick={setDays} />
      <ChannelHealthCard days={days} />
      <ModelLatencyCard days={days} />
      <RevenueCard days={days} />
    </div>
  )
}

function healthVariant(bp: number): 'success' | 'muted' | 'destructive' {
  if (bp >= BAD_BP) return 'destructive'
  if (bp >= WARN_BP) return 'muted'
  return 'success'
}

function ChannelHealthCard({ days }: { days: number }) {
  const { t, i18n } = useTranslation()
  const q = useQuery({
    queryKey: qk.statsChannels(days),
    queryFn: () => apiFetch<{ data: ChannelRow[] }>(`/admin/stats/channels?days=${days}&limit=50`),
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
        ) : (
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
                  <Td className="max-w-48 truncate">{c.name || `#${c.channel_id}`}</Td>
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
      </CardContent>
    </Card>
  )
}

function ModelLatencyCard({ days }: { days: number }) {
  const { t, i18n } = useTranslation()
  const q = useQuery({
    queryKey: qk.statsModels(days),
    queryFn: () => apiFetch<{ data: ModelRow[] }>(`/admin/stats/models?days=${days}&limit=50`),
  })

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('admin:statModels')}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        <p className="text-xs text-muted-foreground">{t('admin:statModelsHint')}</p>
        {q.isError ? (
          <p className="text-sm text-destructive">{describeError(q.error)}</p>
        ) : (
          <Table>
            <THead>
              <Tr>
                <Th>{t('admin:modelName')}</Th>
                <Th>{t('common:requests')}</Th>
                <Th>{t('common:tokens')}</Th>
                <Th>TTFT p50 / p95 / p99</Th>
                <Th>{t('admin:statLatency')} p50 / p95 / p99</Th>
                <Th>{t('admin:statTps')}</Th>
                <Th>{t('common:amount')}</Th>
              </Tr>
            </THead>
            <TBody>
              {(q.data?.data ?? []).map((m) => (
                <Tr key={m.model}>
                  <Td className="max-w-56 truncate font-mono text-xs">{m.model}</Td>
                  <Td>{formatCount(m.requests, i18n.language)}</Td>
                  <Td>{formatCount(m.tokens, i18n.language)}</Td>
                  <Td className="font-mono text-xs">
                    {m.ttft_p50_ms} / {m.ttft_p95_ms} / {m.ttft_p99_ms} ms
                  </Td>
                  <Td className="font-mono text-xs">
                    {m.latency_p50_ms} / {m.latency_p95_ms} / {m.latency_p99_ms} ms
                  </Td>
                  <Td>{formatTokensPerSec(m.tokens_per_1k_sec, i18n.language)}</Td>
                  <Td>{formatMoney(m.amount_micro, i18n.language)}</Td>
                </Tr>
              ))}
            </TBody>
          </Table>
        )}
      </CardContent>
    </Card>
  )
}

function RevenueCard({ days }: { days: number }) {
  const { t, i18n } = useTranslation()
  const q = useQuery({
    queryKey: qk.statsMargin(days),
    queryFn: () => apiFetch<MarginResp>(`/admin/stats/margin?days=${days}`),
  })
  const total = q.data?.total
  const chart = (q.data?.data ?? []).map((d) => ({
    day: d.day.slice(5),
    amount: d.amount_micro / 1_000_000,
    discount: d.discount_micro / 1_000_000,
  }))

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('admin:statRevenue')}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        <p className="text-xs text-muted-foreground">{t('admin:statRevenueHint')}</p>
        {q.isError ? (
          <p className="text-sm text-destructive">{describeError(q.error)}</p>
        ) : (
          <>
            <div className="flex flex-wrap gap-2">
              <Badge variant="muted">
                {t('admin:statAmount')} {formatMoney(total?.amount_micro ?? 0, i18n.language)}
              </Badge>
              <Badge variant="muted">
                {t('admin:statDiscount')} {formatMoney(total?.discount_micro ?? 0, i18n.language)}
              </Badge>
              <Badge variant="muted">
                {t('common:requests')} {formatCount(total?.requests ?? 0, i18n.language)}
              </Badge>
              <Badge variant={healthVariant(total?.error_rate_bp ?? 0)}>
                {t('admin:statErrorRate')} {formatBp(total?.error_rate_bp ?? 0, i18n.language)}
              </Badge>
              {(total?.upstream_cost_micro ?? 0) > 0 ? (
                <Badge variant="success">
                  {t('admin:statMargin')} {formatMoney(total?.margin_micro ?? 0, i18n.language)}
                </Badge>
              ) : (
                <Badge variant="muted">{t('admin:statMarginPending')}</Badge>
              )}
            </div>
            <div className="h-64">
              <ResponsiveContainer width="100%" height="100%">
                <BarChart data={chart}>
                  <CartesianGrid strokeDasharray="3 3" className="stroke-border" />
                  <XAxis dataKey="day" fontSize={12} />
                  <YAxis fontSize={12} />
                  <Tooltip />
                  <Bar dataKey="amount" name={t('admin:statAmount')} fill="var(--color-primary)" />
                  <Bar
                    dataKey="discount"
                    name={t('admin:statDiscount')}
                    fill="var(--color-muted-foreground)"
                  />
                </BarChart>
              </ResponsiveContainer>
            </div>
          </>
        )}
      </CardContent>
    </Card>
  )
}
