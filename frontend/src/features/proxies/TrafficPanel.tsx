import { useQuery } from '@tanstack/react-query'
import dayjs from 'dayjs'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Card } from '@/components/ui/card'
import { Segmented } from '@/components/ui/segmented'
import { Select } from '@/components/ui/select'
import { TableSkeleton } from '@/components/ui/skeleton'
import { EmptyState, ErrorState } from '@/components/ui/state'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { TimeChart } from '@/components/ui/time-chart'
import { byteScale, formatBytes, formatCount } from '@/features/monitor/format'
import { chartColor } from '@/lib/chart'
import { describeError } from '@/lib/i18n'
import { cn } from '@/lib/utils'
import { proxyOptions, proxyTrafficOptions } from './options'
import type { ProxyTraffic, TrafficRange, TrafficResp } from './types'

const RANGES: TrafficRange[] = ['1h', '24h', '7d']

/// 范围内每个桶都给一个点（没流量的桶补 0），图上才看得出空闲时段。
function buckets(resp: TrafficResp, selected: ProxyTraffic[]) {
  const step = resp.bucket_secs
  const now = Math.floor(Date.now() / 1000)
  const first = Math.floor((now - resp.span_secs) / step) * step
  const last = Math.floor(now / step) * step
  const sums = new Map<number, { up: number; down: number }>()
  for (const proxy of selected) {
    for (const p of proxy.points) {
      const sum = sums.get(p.t) ?? { up: 0, down: 0 }
      sum.up += p.up_bytes
      sum.down += p.down_bytes
      sums.set(p.t, sum)
    }
  }
  const points: { t: number; up: number; down: number }[] = []
  for (let t = first; t <= last; t += step) {
    const sum = sums.get(t)
    points.push({ t, up: sum?.up ?? 0, down: sum?.down ?? 0 })
  }
  return points
}

/// 出口代理流量：上下行趋势与各代理汇总。数据每分钟落库一次，最多保留 7 天。
export function TrafficPanel({ tabs }: { tabs: React.ReactNode }) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const [range, setRange] = useState<TrafficRange>('24h')
  const [proxy, setProxy] = useState('')
  const traffic = useQuery(proxyTrafficOptions(range))
  const names = useQuery(proxyOptions())
  const nameOf = (id: number) => names.data?.find((p) => p.id === id)?.name ?? `#${id}`
  const rows = [...(traffic.data?.proxies ?? [])].sort(
    (a, b) => b.up_bytes + b.down_bytes - (a.up_bytes + a.down_bytes),
  )
  const selected = proxy === '' ? rows : rows.filter((r) => String(r.proxy_id) === proxy)
  const total = selected.reduce(
    (sum, r) => ({ up: sum.up + r.up_bytes, down: sum.down + r.down_bytes, requests: sum.requests + r.requests }),
    { up: 0, down: 0, requests: 0 },
  )

  const toolbar = (
    <div className="flex flex-wrap items-center justify-between gap-2">
      {tabs}
      <span className="flex flex-wrap items-center gap-2">
        <Select
          aria-label={t('admin:proxyTrafficProxy')}
          value={proxy}
          onChange={setProxy}
          placeholder={t('admin:proxyTrafficAll')}
          options={rows.map((r) => ({ value: String(r.proxy_id), label: nameOf(r.proxy_id) }))}
          className="w-48"
        />
        <Segmented
          size="sm"
          ariaLabel={t('admin:proxyTrafficRange')}
          value={range}
          onChange={setRange}
          options={RANGES.map((value) => ({ value, label: t(`admin:proxyTrafficRange_${value}`) }))}
        />
      </span>
    </div>
  )

  if (traffic.isError) {
    return (
      <>
        {toolbar}
        <ErrorState message={describeError(traffic.error)} onRetry={() => void traffic.refetch()} />
      </>
    )
  }
  if (traffic.isPending) {
    return (
      <>
        {toolbar}
        <TableSkeleton rows={6} cols={5} />
      </>
    )
  }
  if (rows.length === 0) {
    return (
      <>
        {toolbar}
        <EmptyState title={t('admin:proxyTrafficEmpty')} hint={t('admin:proxyTrafficEmptyHint')} />
      </>
    )
  }

  const points = buckets(traffic.data, selected)
  const scale = byteScale(points.flatMap((p) => [p.up, p.down]))
  const format = (n: number) => `${formatCount(n, locale)} ${scale.unit}`
  const data = points.map((p) => ({
    bucket: dayjs.unix(p.t).format('YYYY-MM-DD HH:mm'),
    down: scale.scale(p.down),
    up: scale.scale(p.up),
  }))
  return (
    <>
      {toolbar}
      <Card className="space-y-4 p-5">
        <div className="grid grid-cols-1 gap-3 sm:grid-cols-3">
          <Total label={t('admin:proxyTrafficDown')} value={formatBytes(total.down, locale)} />
          <Total label={t('admin:proxyTrafficUp')} value={formatBytes(total.up, locale)} />
          <Total label={t('admin:proxyTrafficRequests')} value={formatCount(total.requests, locale)} />
        </div>
        <TimeChart
          stacked
          defaultType="area"
          data={data}
          unit={scale.unit}
          format={format}
          label={t('admin:proxyTrafficChart')}
          partialBucket={data.at(-1)?.bucket}
          series={[
            { key: 'down', label: t('admin:proxyTrafficDown'), color: chartColor(0) },
            { key: 'up', label: t('admin:proxyTrafficUp'), color: chartColor(1) },
          ]}
        />
        <p className="text-xs text-muted-foreground">{t('admin:proxyTrafficBasis')}</p>
      </Card>
      <Table stickyHeader>
        <THead>
          <Tr>
            <Th>{t('admin:proxyName')}</Th>
            <Th numeric>{t('admin:proxyTrafficDown')}</Th>
            <Th numeric>{t('admin:proxyTrafficUp')}</Th>
            <Th numeric>{t('admin:proxyTrafficTotal')}</Th>
            <Th numeric>{t('admin:proxyTrafficRequests')}</Th>
          </Tr>
        </THead>
        <TBody>
          {rows.map((r) => {
            const active = String(r.proxy_id) === proxy
            return (
              <Tr key={r.proxy_id} className={cn(active && 'bg-muted/60')}>
                <Td>
                  <button
                    type="button"
                    aria-pressed={active}
                    className="truncate font-medium hover:text-primary"
                    onClick={() => setProxy(active ? '' : String(r.proxy_id))}
                  >
                    {nameOf(r.proxy_id)}
                  </button>
                </Td>
                <Td numeric>{formatBytes(r.down_bytes, locale)}</Td>
                <Td numeric>{formatBytes(r.up_bytes, locale)}</Td>
                <Td numeric className="font-medium">{formatBytes(r.up_bytes + r.down_bytes, locale)}</Td>
                <Td numeric>{formatCount(r.requests, locale)}</Td>
              </Tr>
            )
          })}
        </TBody>
      </Table>
    </>
  )
}

function Total({ label, value }: { label: string; value: string }) {
  return (
    <div className="rounded-md border border-border/70 px-3 py-2">
      <p className="text-xs text-muted-foreground">{label}</p>
      <p className="text-lg font-semibold tabular-nums">{value}</p>
    </div>
  )
}
