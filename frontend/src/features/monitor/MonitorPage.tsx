import { useQuery } from '@tanstack/react-query'
import dayjs from 'dayjs'
import { Activity, Cpu, Gauge, HardDrive, MemoryStick, Network, RotateCw, ScrollText, TrendingUp } from 'lucide-react'
import { useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { PageHeader } from '@/components/ui/page'
import { SearchInput } from '@/components/ui/search-input'
import { Segmented } from '@/components/ui/segmented'
import { Stat } from '@/components/ui/stat'
import { EmptyState, ErrorState, LoadingState } from '@/components/ui/state'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { Tabs } from '@/components/ui/tabs'
import { TimeChart } from '@/components/ui/time-chart'
import { apiFetch } from '@/lib/api'
import { chartColor } from '@/lib/chart'
import { describeError } from '@/lib/i18n'
import { cn } from '@/lib/utils'
import { byteScale, formatBytes, formatCount, formatPercent, formatRate, formatUptime, ratio, usageTone } from './format'
import type { LogsResp, Overview, Probe, Sample, TableSize } from './types'

const TABS = ['overview', 'trends', 'logs'] as const
type MonitorTab = (typeof TABS)[number]
const REFRESH = [0, 10, 30] as const

/// 运维监控：服务器压力、中间件占用、24 小时趋势、告警日志。只看不改——会改数据的
/// 运维动作在「运维操作」页。服务器数字是控制台所在机器的（单机部署即整台机器），
/// 趋势由 worker 每分钟采一次。
export function MonitorPage() {
  const { t } = useTranslation()
  const [tab, setTab] = useState<MonitorTab>('overview')
  const [refresh, setRefresh] = useState<(typeof REFRESH)[number]>(10)
  const interval = refresh === 0 ? false : refresh * 1000

  const items = [
    { id: 'overview', label: t('monitor:tabOverview'), icon: Gauge },
    { id: 'trends', label: t('monitor:tabTrends'), icon: TrendingUp },
    { id: 'logs', label: t('monitor:tabLogs'), icon: ScrollText },
  ]
  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        title={t('monitor:title')}
        description={t('monitor:desc')}
        icon={Activity}
        action={
          <Segmented
            size="sm"
            ariaLabel={t('monitor:autoRefresh')}
            value={refresh}
            onChange={setRefresh}
            options={REFRESH.map((s) => ({ value: s, label: s === 0 ? t('monitor:refreshOff') : t('monitor:refreshEvery', { s }) }))}
          />
        }
      />
      <Tabs variant="underline" items={items} active={tab} onChange={(id) => setTab(id as MonitorTab)} />
      {tab === 'overview' && <OverviewTab interval={interval} />}
      {tab === 'trends' && <TrendsTab interval={interval} />}
      {tab === 'logs' && <LogsTab interval={interval} />}
    </div>
  )
}

function OverviewTab({ interval }: { interval: number | false }) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const q = useQuery({
    queryKey: ['admin', 'monitor', 'overview'],
    queryFn: () => apiFetch<Overview>('/admin/monitor/overview'),
    refetchInterval: interval,
  })
  if (q.isPending) return <LoadingState />
  if (q.isError) return <ErrorState message={describeError(q.error)} onRetry={() => void q.refetch()} />
  const d = q.data
  const { host, rates } = d
  const memUsed = host.memory ? host.memory.total_bytes - host.memory.available_bytes : null
  const memPct = ratio(memUsed, host.memory?.total_bytes)
  const diskUsed = host.disk ? host.disk.total_bytes - host.disk.free_bytes : null
  const diskPct = ratio(diskUsed, host.disk?.total_bytes)
  const loadPct = host.load && host.cpus ? (host.load[0] / host.cpus) * 100 : null
  const na = t('monitor:unsupported')

  return (
    <div className="flex flex-col gap-4">
      <section aria-labelledby="monitor-host" className="flex flex-col gap-3">
        <div className="flex flex-wrap items-baseline justify-between gap-2">
          <h2 id="monitor-host" className="text-sm font-semibold">{t('monitor:hostTitle')}</h2>
          <p className="text-xs text-muted-foreground">
            {t('monitor:hostMeta', { node: d.node, time: dayjs(d.collected_at).format('HH:mm:ss') })}
            {q.isFetching && <RotateCw className="ml-1.5 inline h-3 w-3 animate-spin" aria-hidden />}
          </p>
        </div>
        <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-4">
          <Stat icon={Cpu} layout="stacked" label={t('monitor:cpu')} tone={usageTone(rates.cpu_percent)} value={rates.cpu_percent == null ? na : formatPercent(rates.cpu_percent, locale)}
            sub={<UsageSub text={host.cpus ? t('monitor:cores', { n: host.cpus }) : ''} percent={rates.cpu_percent} />} />
          <Stat icon={MemoryStick} layout="stacked" label={t('monitor:memory')} tone={usageTone(memPct)} value={memPct == null ? na : formatPercent(memPct, locale)}
            sub={<UsageSub text={host.memory ? `${formatBytes(memUsed, locale)} / ${formatBytes(host.memory.total_bytes, locale)}` : ''} percent={memPct} />} />
          <Stat icon={HardDrive} layout="stacked" label={t('monitor:disk')} tone={usageTone(diskPct)} value={diskPct == null ? na : formatPercent(diskPct, locale)}
            sub={<UsageSub text={host.disk ? t('monitor:freeOf', { free: formatBytes(host.disk.free_bytes, locale), total: formatBytes(host.disk.total_bytes, locale) }) : ''} percent={diskPct} />} />
          <Stat icon={Network} layout="stacked" label={t('monitor:network')} value={rates.net_rx_bps == null ? na : `↓ ${formatRate(rates.net_rx_bps, locale)}`}
            sub={rates.net_tx_bps == null ? undefined : `↑ ${formatRate(rates.net_tx_bps, locale)}`} />
        </div>
        <dl className="grid gap-x-6 gap-y-1 rounded-lg border border-border bg-card px-4 py-3 text-xs sm:grid-cols-2">
          <Kv label={t('monitor:load')} value={host.load ? `${host.load.map((l) => l.toFixed(2)).join(' / ')}${loadPct == null ? '' : ` · ${t('monitor:perCore', { v: formatPercent(loadPct, locale, 0) })}`}` : na} />
          <Kv label={t('monitor:swap')} value={host.memory?.swap_total_bytes ? t('monitor:usedOf', { used: formatBytes(host.memory.swap_total_bytes - host.memory.swap_free_bytes, locale), total: formatBytes(host.memory.swap_total_bytes, locale) }) : '—'} />
          <Kv label={t('monitor:processRss')} value={host.process ? `${formatBytes(host.process.rss_bytes, locale)} · ${t('monitor:threads', { n: host.process.threads })}${host.process.open_fds == null ? '' : ` · ${t('monitor:fds', { n: host.process.open_fds })}`}` : na} />
          <Kv label={t('monitor:uptime')} value={formatUptime(host.uptime_secs, t)} />
        </dl>
      </section>

      <section aria-labelledby="monitor-mid" className="flex flex-col gap-3">
        <h2 id="monitor-mid" className="text-sm font-semibold">{t('monitor:middlewareTitle')}</h2>
        <div className="grid gap-3 lg:grid-cols-2">
          <ProbeCard name="PostgreSQL" probe={d.postgres} version={d.postgres.version} uptime={d.postgres.uptime_secs}
            meter={{ label: t('monitor:connections'), used: d.postgres.connections, total: d.postgres.max_connections, text: `${d.postgres.connections ?? '—'} / ${d.postgres.max_connections ?? '—'}` }}
            rows={[
              [t('monitor:pgActive'), `${formatCount(d.postgres.active, locale)} · ${t('monitor:pgIdleTx', { n: d.postgres.idle_in_transaction ?? 0 })}`],
              [t('monitor:pool'), d.postgres.pool ? t('monitor:poolValue', d.postgres.pool) : '—'],
              [t('monitor:dbSize'), formatBytes(d.postgres.database_bytes, locale)],
              [t('monitor:cacheHit'), d.postgres.cache_hit_ratio == null ? '—' : formatPercent(d.postgres.cache_hit_ratio * 100, locale, 2)],
              [t('monitor:longestQuery'), d.postgres.longest_query_secs == null ? '—' : t('monitor:seconds', { n: d.postgres.longest_query_secs.toFixed(1) })],
              [t('monitor:deadlocks'), formatCount(d.postgres.deadlocks, locale)],
            ]}
            tables={d.postgres.tables} />
          <ProbeCard name="Redis" probe={d.redis} version={d.redis.version} uptime={d.redis.uptime_secs}
            meter={d.redis.maxmemory ? { label: t('monitor:memory'), used: d.redis.used_memory, total: d.redis.maxmemory, text: t('monitor:usedOf', { used: formatBytes(d.redis.used_memory, locale), total: formatBytes(d.redis.maxmemory, locale) }) } : undefined}
            rows={[
              [t('monitor:memory'), d.redis.maxmemory ? `${formatBytes(d.redis.used_memory, locale)}` : t('monitor:redisNoLimit', { used: formatBytes(d.redis.used_memory, locale) })],
              [t('monitor:redisRss'), `${formatBytes(d.redis.used_memory_rss, locale)} · ${t('monitor:fragmentation', { n: d.redis.fragmentation_ratio ?? '—' })}`],
              [t('monitor:clients'), formatCount(d.redis.clients, locale)],
              [t('monitor:opsPerSec'), formatCount(d.redis.ops_per_sec, locale)],
              [t('monitor:keys'), formatCount(d.redis.keys, locale)],
              [t('monitor:hitRatio'), d.redis.hit_ratio == null ? '—' : formatPercent(d.redis.hit_ratio * 100, locale)],
              [t('monitor:evicted'), formatCount(d.redis.evicted_keys, locale)],
              [t('monitor:persistence'), d.redis.rdb_last_bgsave_status ?? '—'],
            ]} />
          <ProbeCard name="ClickHouse" probe={d.clickhouse} version={d.clickhouse.version} uptime={d.clickhouse.uptime_secs}
            meter={(() => {
              const disk = d.clickhouse.disks?.[0]
              if (!disk?.total_bytes || disk.free_bytes == null) return undefined
              return { label: t('monitor:disk'), used: disk.total_bytes - disk.free_bytes, total: disk.total_bytes, text: t('monitor:freeOf', { free: formatBytes(disk.free_bytes, locale), total: formatBytes(disk.total_bytes, locale) }) }
            })()}
            rows={[
              [t('monitor:memory'), formatBytes(d.clickhouse.memory_resident, locale)],
              [t('monitor:dbSize'), `${formatBytes(d.clickhouse.database_bytes, locale)} · ${t('monitor:parts', { n: d.clickhouse.parts ?? 0 })}`],
              [t('monitor:runningQueries'), formatCount(d.clickhouse.queries, locale)],
              [t('monitor:connections'), `TCP ${formatCount(d.clickhouse.tcp_connections, locale)} · HTTP ${formatCount(d.clickhouse.http_connections, locale)}`],
              [t('monitor:merges'), formatCount(d.clickhouse.background_merges, locale)],
            ]}
            tables={d.clickhouse.tables} />
          <ProbeCard name="NATS" probe={d.nats} version={d.nats.version}
            meter={d.nats.jetstream?.max_storage ? { label: t('monitor:jsStorage'), used: d.nats.jetstream.storage_bytes, total: d.nats.jetstream.max_storage, text: formatBytes(d.nats.jetstream.storage_bytes, locale) } : undefined}
            rows={[
              [t('monitor:natsState'), d.nats.state ?? '—'],
              [t('monitor:jsStorage'), d.nats.jetstream?.error ? d.nats.jetstream.error : `${formatBytes(d.nats.jetstream?.storage_bytes, locale)} · ${t('monitor:jsMemory', { v: formatBytes(d.nats.jetstream?.memory_bytes, locale) })}`],
              [t('monitor:jsStreams'), `${formatCount(d.nats.jetstream?.streams, locale)} · ${t('monitor:jsConsumers', { n: d.nats.jetstream?.consumers ?? 0 })}`],
              [t('monitor:natsTraffic'), `↓ ${formatBytes(d.nats.in_bytes, locale)} · ↑ ${formatBytes(d.nats.out_bytes, locale)}`],
              [t('monitor:reconnects'), formatCount(d.nats.reconnects, locale)],
            ]}
            tables={d.nats.streams?.map((s) => ({ name: s.name, bytes: s.bytes, rows: s.messages }))} />
        </div>
      </section>
    </div>
  )
}

/// 占用条 + 状态文字：颜色按状态，旁边恒有「正常 / 偏高 / 告急」——状态不只靠颜色表达。
function UsageSub({ text, percent }: { text: string; percent: number | null | undefined }) {
  const { t } = useTranslation()
  const tone = usageTone(percent)
  const clamped = Math.max(0, Math.min(100, percent ?? 0))
  return (
    <span className="flex w-full flex-col gap-1.5">
      {text && <span>{text}</span>}
      {percent != null && (
        <span className="flex items-center gap-2">
          <span className="h-1.5 flex-1 overflow-hidden rounded-full bg-muted" role="meter" aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(clamped)} aria-label={t('monitor:usage')}>
            <span className={cn('block h-full rounded-full', toneBg(tone))} style={{ width: `${clamped}%` }} />
          </span>
          <span className={cn('shrink-0 text-[11px]', tone === 'bad' ? 'text-destructive' : tone === 'warn' ? 'text-warning' : 'text-muted-foreground')}>{t(`monitor:tone_${tone}`)}</span>
        </span>
      )}
    </span>
  )
}

function toneBg(tone: ReturnType<typeof usageTone>): string {
  return tone === 'bad' ? 'bg-destructive' : tone === 'warn' ? 'bg-warning' : 'bg-success'
}

function Kv({ label, value }: { label: string; value: React.ReactNode }) {
  return (
    <div className="flex min-w-0 items-baseline justify-between gap-3 py-1 sm:justify-start">
      <dt className="shrink-0 text-muted-foreground">{label}</dt>
      <dd className="min-w-0 truncate text-right font-medium tabular-nums sm:text-left" title={typeof value === 'string' ? value : undefined}>{value}</dd>
    </div>
  )
}

function ProbeCard({ name, probe, version, uptime, meter, rows, tables }: {
  name: string
  probe: Probe
  version?: string
  uptime?: number
  meter?: { label: string; used: number | null | undefined; total: number | null | undefined; text: string }
  rows: [string, React.ReactNode][]
  tables?: TableSize[]
}) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const status = probe.configured === false
    ? <Badge variant="muted" dot>{t('monitor:notConfigured')}</Badge>
    : probe.ok ? <Badge variant="success" dot>{t('monitor:healthy')}</Badge> : <Badge variant="destructive" dot>{t('monitor:unreachable')}</Badge>
  const pct = meter ? ratio(meter.used, meter.total) : null
  return (
    <Card>
      <CardHeader className="flex flex-row flex-wrap items-center justify-between gap-2">
        <CardTitle className="flex items-center gap-2 text-base">{name}{status}</CardTitle>
        {probe.ok && <span className="text-xs text-muted-foreground">{[version && `v${version}`, uptime != null && t('monitor:upFor', { v: formatUptime(uptime, t) })].filter(Boolean).join(' · ')}</span>}
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        {probe.configured === false ? <p className="text-sm text-muted-foreground">{t('monitor:notConfiguredHint')}</p>
          : !probe.ok ? <ErrorState message={t('monitor:probeFailed', { error: probe.error ?? '—' })} />
            : <>
              {meter && pct != null && (
                <div className="flex flex-col gap-1.5">
                  <div className="flex items-baseline justify-between gap-2 text-xs">
                    <span className="text-muted-foreground">{meter.label}</span>
                    <span className="font-medium tabular-nums">{meter.text} · {formatPercent(pct, locale)}</span>
                  </div>
                  <div className="h-2 overflow-hidden rounded-full bg-muted" role="meter" aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(pct)} aria-label={meter.label}>
                    <div className={cn('h-full rounded-full', toneBg(usageTone(pct)))} style={{ width: `${Math.min(100, pct)}%` }} />
                  </div>
                </div>
              )}
              <dl className="grid gap-x-6 text-xs sm:grid-cols-2">
                {rows.map(([label, value]) => <Kv key={label} label={label} value={value} />)}
              </dl>
              {tables && tables.length > 0 && (
                <Table dense aria-label={t('monitor:largestTables', { name })} className="tabular-nums">
                  <THead><Tr><Th>{t('monitor:tableName')}</Th><Th numeric>{t('monitor:tableRows')}</Th><Th numeric>{t('monitor:tableSize')}</Th></Tr></THead>
                  <TBody>{tables.slice(0, 6).map((row) => <Tr key={row.name}><Td className="max-w-48 truncate font-mono text-xs">{row.name}</Td><Td numeric>{formatCount(row.rows, locale)}</Td><Td numeric>{formatBytes(row.bytes, locale)}</Td></Tr>)}</TBody>
                </Table>
              )}
            </>}
      </CardContent>
    </Card>
  )
}

const HOURS = [1, 6, 24] as const

function TrendsTab({ interval }: { interval: number | false }) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const [hours, setHours] = useState<(typeof HOURS)[number]>(6)
  const q = useQuery({
    queryKey: ['admin', 'monitor', 'history', hours],
    queryFn: () => apiFetch<{ data: Sample[] }>(`/admin/monitor/history?hours=${hours}`),
    // 一分钟一个点：刷新再快也没有新数据
    refetchInterval: interval === false ? false : 60_000,
  })
  const header = (
    <div className="flex flex-wrap items-center justify-between gap-2">
      <p className="text-xs text-muted-foreground">{t('monitor:trendHint')}</p>
      <Segmented size="sm" ariaLabel={t('monitor:window')} value={hours} onChange={setHours} options={HOURS.map((h) => ({ value: h, label: t('monitor:hours', { n: h }) }))} />
    </div>
  )
  if (q.isPending) return <>{header}<LoadingState /></>
  if (q.isError) return <>{header}<ErrorState message={describeError(q.error)} onRetry={() => void q.refetch()} /></>
  const samples = q.data.data
  if (samples.length === 0) return <>{header}<EmptyState title={t('monitor:noSamples')} hint={t('monitor:noSamplesHint')} /></>
  const point = (s: Sample) => ({ bucket: dayjs.unix(s.t).format('YYYY-MM-DD HH:mm') })
  // 字节类纵轴按窗口内最大值换成 KB / MB / GB，刻度才读得懂
  const net = byteScale(samples.flatMap((s) => [s.net_rx, s.net_tx]))
  const mem = byteScale(samples.flatMap((s) => [s.redis_mem, s.ch_mem]))
  const charts = [
    {
      title: t('monitor:chartUsage'), unit: '%', percent: true, format: (v: number) => formatPercent(v, locale),
      series: [
        { key: 'cpu', label: t('monitor:cpu'), color: chartColor(0) },
        { key: 'mem', label: t('monitor:memory'), color: chartColor(1) },
        { key: 'disk', label: t('monitor:disk'), color: chartColor(2) },
      ],
      data: samples.map((s) => ({ ...point(s), cpu: s.cpu, mem: ratio(s.mem_used, s.mem_total), disk: ratio(s.disk_used, s.disk_total) })),
    },
    {
      title: t('monitor:chartNetwork'), unit: `${net.unit}/s`, format: (v: number) => `${formatCount(v, locale)} ${net.unit}/s`,
      series: [
        { key: 'rx', label: t('monitor:netIn'), color: chartColor(0) },
        { key: 'tx', label: t('monitor:netOut'), color: chartColor(1) },
      ],
      data: samples.map((s) => ({ ...point(s), rx: net.scale(s.net_rx), tx: net.scale(s.net_tx) })),
    },
    {
      title: t('monitor:chartConnections'), unit: t('monitor:unitConnections'), format: (v: number) => formatCount(v, locale),
      series: [
        { key: 'pg', label: t('monitor:pgConnections'), color: chartColor(0) },
        { key: 'redis', label: t('monitor:redisClients'), color: chartColor(1) },
      ],
      data: samples.map((s) => ({ ...point(s), pg: s.pg_conns, redis: s.redis_clients })),
    },
    {
      title: t('monitor:chartMemory'), unit: mem.unit, format: (v: number) => `${formatCount(v, locale)} ${mem.unit}`,
      series: [
        { key: 'redis', label: 'Redis', color: chartColor(0) },
        { key: 'ch', label: 'ClickHouse', color: chartColor(1) },
      ],
      data: samples.map((s) => ({ ...point(s), redis: mem.scale(s.redis_mem), ch: mem.scale(s.ch_mem) })),
    },
  ]
  return (
    <div className="flex flex-col gap-3">
      {header}
      <div className="grid gap-3 xl:grid-cols-2">
        {charts.map((c) => (
          <Card key={c.title}>
            <CardHeader><CardTitle className="text-base">{c.title}</CardTitle></CardHeader>
            <CardContent>
              <TimeChart compact line percent={c.percent} data={c.data} series={c.series} unit={c.unit} label={c.title} format={c.format} />
            </CardContent>
          </Card>
        ))}
      </div>
    </div>
  )
}

function LogsTab({ interval }: { interval: number | false }) {
  const { t } = useTranslation()
  const [level, setLevel] = useState<'all' | 'error'>('all')
  const [search, setSearch] = useState('')
  // 停手 300ms 再查：每次按键都去 Redis 读一遍全量日志没有意义
  const [term, setTerm] = useState('')
  useEffect(() => {
    const timer = setTimeout(() => setTerm(search.trim()), 300)
    return () => clearTimeout(timer)
  }, [search])
  const q = useQuery({
    queryKey: ['admin', 'monitor', 'logs', level, term],
    queryFn: () => apiFetch<LogsResp>(`/admin/monitor/logs?limit=500${level === 'error' ? '&level=error' : ''}${term ? `&q=${encodeURIComponent(term)}` : ''}`),
    refetchInterval: interval,
  })
  return (
    <div className="flex flex-col gap-3">
      <div className="flex flex-wrap items-center gap-2">
        <Segmented size="sm" ariaLabel={t('monitor:logLevel')} value={level} onChange={setLevel}
          options={[{ value: 'all', label: t('monitor:levelAll') }, { value: 'error', label: t('monitor:levelError') }]} />
        <SearchInput className="w-full sm:w-72" value={search} onChange={setSearch} placeholder={t('monitor:logSearch')} aria-label={t('monitor:logSearch')} />
        {q.data && <span className="text-xs text-muted-foreground sm:ml-auto">{t('monitor:logSummary', { total: q.data.total, errors: q.data.errors })}</span>}
      </div>
      {q.data?.source === 'local' && <p role="status" className="text-xs text-warning">{t('monitor:logLocalOnly')}</p>}
      {q.isPending ? <LoadingState />
        : q.isError ? <ErrorState message={describeError(q.error)} onRetry={() => void q.refetch()} />
          : q.data.data.length === 0 ? <EmptyState title={t('monitor:noLogs')} hint={t('monitor:noLogsHint')} />
            : (
              <Table dense stickyHeader wrapperClassName="max-h-[70dvh]" aria-label={t('monitor:tabLogs')}>
                <THead><Tr><Th className="w-36">{t('monitor:logTime')}</Th><Th className="w-20">{t('monitor:logLevel')}</Th><Th className="w-40">{t('monitor:logSource')}</Th><Th>{t('monitor:logMessage')}</Th></Tr></THead>
                <TBody>
                  {q.data.data.map((e, i) => (
                    <Tr key={`${e.ts}-${i}`}>
                      <Td className="whitespace-nowrap align-top text-xs tabular-nums text-muted-foreground" title={e.ts}>{dayjs(e.ts).format('MM-DD HH:mm:ss')}</Td>
                      <Td className="align-top"><Badge variant={e.level === 'ERROR' ? 'destructive' : 'warning'}>{e.level}</Badge></Td>
                      <Td className="align-top text-xs"><div className="truncate" title={e.node}>{e.node || '—'}</div><div className="text-muted-foreground">{e.role}</div></Td>
                      <Td className="align-top"><div className="font-mono text-xs break-all whitespace-pre-wrap">{e.message}</div><div className="mt-0.5 font-mono text-[11px] text-muted-foreground">{e.target}</div></Td>
                    </Tr>
                  ))}
                </TBody>
              </Table>
            )}
    </div>
  )
}
