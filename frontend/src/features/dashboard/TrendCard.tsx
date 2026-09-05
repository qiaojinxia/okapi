import { useQuery } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { useState } from 'react'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { EmptyState, ErrorState, LoadingState } from '@/components/ui/state'
import { Segmented } from '@/components/ui/segmented'
import { TimeChart } from '@/components/ui/time-chart'
import type { MarginResp } from './types'
import { calendarDays } from '@/features/portal-overview/usage-chart-data'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { qk } from '@/lib/query-keys'

export function TrendCard({ days }: { days: number }) {
  const { t, i18n } = useTranslation()
  const [metric, setMetric] = useState<'requests' | 'amount'>('requests')
  const q = useQuery({ queryKey: qk.statsMargin(days), queryFn: () => apiFetch<MarginResp>(`/admin/stats/margin?days=${days}`) })
  const end = q.data?.window?.end_date ?? new Date().toISOString().slice(0, 10)
  const start = q.data?.window?.start_date ?? new Date(new Date(`${end}T00:00:00Z`).getTime() - (days - 1) * 86400_000).toISOString().slice(0, 10)
  const lookup = new Map((q.data?.data ?? []).map((row) => [row.day, row]))
  const data = calendarDays(start, end).map((bucket) => ({ bucket, value: metric === 'requests' ? lookup.get(bucket)?.requests ?? 0 : (lookup.get(bucket)?.amount_micro ?? 0) / 1_000_000 }))
  const label = t(`charts:metric_${metric}`)
  return <Card><CardHeader className="flex flex-wrap items-center justify-between gap-3"><CardTitle>{t('admin:trendTitle')}</CardTitle><Segmented ariaLabel={t('charts:metric')} value={metric} onChange={setMetric} options={(['requests', 'amount'] as const).map((value) => ({ value, label: t(`charts:metric_${value}`) }))} /></CardHeader>
    <CardContent>{q.isPending ? <LoadingState /> : q.isError ? <ErrorState message={describeError(q.error)} onRetry={() => void q.refetch()} /> : !q.data?.data.length ? <EmptyState hint={t('admin:trendEmptyHint')} /> : <TimeChart key={metric} data={data} label={t('admin:trendTitle')} unit={metric === 'amount' ? 'USD' : label} format={(v) => metric === 'amount' ? new Intl.NumberFormat(i18n.language, { style: 'currency', currency: 'USD', maximumFractionDigits: 4 }).format(v) : v.toLocaleString(i18n.language)} series={[{ key: 'value', label, color: metric === 'amount' ? 'var(--color-success)' : 'var(--color-primary)' }]} />}</CardContent>
  </Card>
}
