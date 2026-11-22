import { useQuery } from '@tanstack/react-query'
import dayjs from 'dayjs'
import {
  Activity,
  AlertTriangle,
  Check,
  Code2,
  Copy,
  DatabaseZap,
  Layers,
  RotateCw,
  ShieldCheck,
  Zap,
} from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Stat } from '@/components/ui/stat'
import { EmptyState, ErrorState, LoadingState } from '@/components/ui/state'
import { TBody, THead, Table, Td, Th, Tr } from '@/components/ui/table'
import { apiFetch, getKey } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { qk } from '@/lib/query-keys'
import { errorRateTone, formatCount, formatMs, formatPercent } from './format'
import type { MetricsResp } from './types'

async function fetchRawPrometheus(): Promise<string> {
  const key = getKey()
  const headers: Record<string, string> = {}
  if (key) headers.Authorization = `Bearer ${key}`
  const resp = await fetch('/admin/monitor/metrics/raw', {
    credentials: 'same-origin',
    headers,
  })
  if (!resp.ok) throw new Error(`HTTP ${resp.status}`)
  return resp.text()
}

export function MetricsTab({ interval }: { interval: number | false }) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const [showRaw, setShowRaw] = useState(false)
  const [copied, setCopied] = useState(false)

  const q = useQuery({
    queryKey: qk.monitorMetrics,
    queryFn: () => apiFetch<MetricsResp>('/admin/monitor/metrics'),
    refetchInterval: interval,
  })
  const rawQ = useQuery({
    queryKey: [...qk.monitorMetrics, 'raw'] as const,
    queryFn: fetchRawPrometheus,
    enabled: showRaw,
    refetchInterval: showRaw ? interval : false,
  })

  if (q.isPending) return <LoadingState />
  if (q.isError) return <ErrorState message={describeError(q.error)} onRetry={() => void q.refetch()} />

  const d = q.data
  const s = d.summary
  const time = dayjs(d.collected_at).format('HH:mm:ss')
  const authTone =
    s.auth_cache_hit_ratio == null ? 'default' : s.auth_cache_hit_ratio >= 85 ? 'good' : s.auth_cache_hit_ratio >= 60 ? 'warn' : 'bad'
  const settleTone = s.settle_shedding ? 'bad' : s.settle_backlog > 100 ? 'warn' : 'good'

  const copyRaw = async () => {
    if (!rawQ.data) return
    try {
      await navigator.clipboard.writeText(rawQ.data)
      setCopied(true)
      setTimeout(() => setCopied(false), 1800)
    } catch {
      // Clipboard API may be unavailable in non-HTTPS contexts
    }
  }

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <p className="text-xs text-muted-foreground">
          {d.source === 'shared'
            ? t('monitor:metricsSourceShared', { count: d.node_count, time })
            : t('monitor:metricsSourceLocal', { time })}
          {q.isFetching && <RotateCw className="ml-1.5 inline h-3 w-3 animate-spin" aria-hidden />}
        </p>
        <div className="flex items-center gap-2">
          <Button size="sm" variant="outline" onClick={() => setShowRaw((v) => !v)}>
            <Code2 className="mr-1.5 h-3.5 w-3.5" aria-hidden />
            {showRaw ? t('monitor:metricsHideRaw') : t('monitor:metricsViewRaw')}
          </Button>
        </div>
      </div>

      {showRaw && (
        <Card>
          <CardHeader className="flex flex-row flex-wrap items-center justify-between gap-2">
            <CardTitle className="text-sm font-medium">
              {t('monitor:metricsRawHint', { path: d.prometheus_path })}
            </CardTitle>
            <Button size="sm" variant="ghost" disabled={!rawQ.data} onClick={() => void copyRaw()}>
              {copied ? <Check className="mr-1.5 h-3.5 w-3.5" aria-hidden /> : <Copy className="mr-1.5 h-3.5 w-3.5" aria-hidden />}
              {copied ? t('monitor:metricsCopied') : t('monitor:metricsCopyRaw')}
            </Button>
          </CardHeader>
          <CardContent>
            {rawQ.isPending ? (
              <LoadingState />
            ) : rawQ.isError ? (
              <ErrorState message={describeError(rawQ.error)} onRetry={() => void rawQ.refetch()} />
            ) : (
              <pre className="max-h-72 overflow-auto rounded-lg bg-muted/60 p-3 font-mono text-[11px] leading-relaxed">
                {rawQ.data}
              </pre>
            )}
          </CardContent>
        </Card>
      )}

      <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-3">
        <Stat
          icon={Activity}
          layout="stacked"
          label={t('monitor:metricsGatewayRps')}
          value={`${formatCount(s.gateway_rps, locale)} req/s`}
          sub={t('monitor:metricsConsoleRps', {
            rps: formatCount(s.console_rps, locale),
            total: formatCount(s.gateway_requests_total, locale),
          })}
        />
        <Stat
          icon={Zap}
          layout="stacked"
          label={t('monitor:metricsInFlight')}
          value={formatCount(s.gateway_in_flight + s.console_in_flight, locale)}
          sub={t('monitor:metricsInFlightSub', {
            gw: formatCount(s.gateway_in_flight, locale),
            console: formatCount(s.console_in_flight, locale),
          })}
        />
        <Stat
          icon={AlertTriangle}
          layout="stacked"
          label={t('monitor:metricsError5xx')}
          tone={errorRateTone(s.gateway_5xx_rate)}
          value={s.gateway_5xx_rate == null ? '—' : formatPercent(s.gateway_5xx_rate, locale, 2)}
          sub={t('monitor:metricsError5xxSub', {
            errors: formatCount(s.gateway_5xx_total, locale),
            total: formatCount(s.gateway_requests_total, locale),
          })}
        />
        <Stat
          icon={ShieldCheck}
          layout="stacked"
          label={t('monitor:metricsAuthCache')}
          tone={authTone}
          value={s.auth_cache_hit_ratio == null ? '—' : formatPercent(s.auth_cache_hit_ratio, locale)}
          sub={t('monitor:metricsAuthCacheSub', {
            hits: formatCount(s.auth_cache_hits, locale),
            misses: formatCount(s.auth_cache_misses, locale),
          })}
        />
        <Stat
          icon={DatabaseZap}
          layout="stacked"
          label={t('monitor:metricsSettlement')}
          tone={settleTone}
          value={
            <span className="inline-flex items-center gap-2">
              {formatCount(s.settle_backlog, locale)}
              {s.settle_shedding && <Badge variant="destructive">{t('monitor:metricsShedding')}</Badge>}
            </span>
          }
          sub={t('monitor:metricsSettleSub', {
            inFlight: formatCount(s.settle_in_flight, locale),
            p95: formatMs(s.settle_batch_duration_ms.p95, locale),
          })}
        />
        <Stat
          icon={Layers}
          layout="stacked"
          label={t('monitor:metricsFailoverQuality')}
          value={`${formatCount(s.upstream_failovers, locale)} / ${formatCount(s.pool_quality_events_total, locale)}`}
          sub={t('monitor:metricsFailoverSub', {
            failovers: formatCount(s.upstream_failovers, locale),
            rate: formatCount(s.failovers_per_min, locale),
            quality: formatCount(s.pool_quality_events_total, locale),
          })}
        />
      </div>

      <div className="flex flex-wrap gap-x-6 gap-y-1 rounded-xl border border-border bg-card px-4 py-2.5 text-xs text-muted-foreground shadow-card tabular-nums">
        <span>
          {t('monitor:metricsRoutingInv', {
            local: formatCount(s.routing_invalidations_local, locale),
            broadcast: formatCount(s.routing_invalidations_broadcast, locale),
          })}
        </span>
        <span>
          {t('monitor:metricsSettleBatches', {
            ok: formatCount(s.settle_batches_ok, locale),
            fallback: formatCount(s.settle_batches_fallback, locale),
            size: s.settle_batch_size.avg == null ? '—' : formatCount(s.settle_batch_size.avg, locale),
          })}
        </span>
      </div>

      <Card>
        <CardHeader>
          <CardTitle className="text-base">{t('monitor:metricsProvidersTitle')}</CardTitle>
        </CardHeader>
        <CardContent>
          {d.providers.length === 0 ? (
            <EmptyState title={t('monitor:metricsNoProviders')} />
          ) : (
            <Table dense className="tabular-nums" aria-label={t('monitor:metricsProvidersTitle')}>
              <THead>
                <Tr>
                  <Th>{t('monitor:metricsProvider')}</Th>
                  <Th numeric>{t('monitor:metricsRequests')}</Th>
                  <Th numeric>{t('monitor:metricsSuccessRate')}</Th>
                  <Th numeric>TTFT P50</Th>
                  <Th numeric>TTFT P95</Th>
                  <Th numeric>TTFT P99</Th>
                  <Th>{t('monitor:metricsOutcomes')}</Th>
                </Tr>
              </THead>
              <TBody>
                {d.providers.map((p) => (
                  <Tr key={p.provider}>
                    <Td className="font-mono text-xs font-medium">{p.provider}</Td>
                    <Td numeric>{formatCount(p.total, locale)}</Td>
                    <Td numeric>
                      {p.success_ratio == null ? (
                        '—'
                      ) : (
                        <Badge variant={p.success_ratio >= 98 ? 'success' : p.success_ratio >= 90 ? 'warning' : 'destructive'}>
                          {formatPercent(p.success_ratio, locale)}
                        </Badge>
                      )}
                    </Td>
                    <Td numeric>{formatMs(p.ttft_ms.p50, locale)}</Td>
                    <Td numeric>{formatMs(p.ttft_ms.p95, locale)}</Td>
                    <Td numeric>{formatMs(p.ttft_ms.p99, locale)}</Td>
                    <Td className="text-xs text-muted-foreground">
                      {Object.entries(p.outcomes)
                        .map(([k, v]) => `${k}:${formatCount(v, locale)}`)
                        .join(' · ')}
                    </Td>
                  </Tr>
                ))}
              </TBody>
            </Table>
          )}
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle className="text-base">{t('monitor:metricsRoutesTitle')}</CardTitle>
        </CardHeader>
        <CardContent>
          {d.routes.length === 0 ? (
            <EmptyState title={t('monitor:metricsNoRoutes')} />
          ) : (
            <Table dense className="tabular-nums" aria-label={t('monitor:metricsRoutesTitle')}>
              <THead>
                <Tr>
                  <Th>{t('monitor:metricsRole')}</Th>
                  <Th>{t('monitor:metricsRoute')}</Th>
                  <Th numeric>{t('monitor:metricsRequests')}</Th>
                  <Th numeric>2xx / 4xx / 5xx</Th>
                  <Th numeric>{t('monitor:metricsAvg')}</Th>
                  <Th numeric>P50</Th>
                  <Th numeric>P95</Th>
                  <Th numeric>P99</Th>
                </Tr>
              </THead>
              <TBody>
                {d.routes.map((r) => (
                  <Tr key={`${r.role}:${r.route}`}>
                    <Td>
                      <Badge variant={r.role === 'gateway' ? 'default' : 'muted'}>{r.role}</Badge>
                    </Td>
                    <Td className="max-w-64 truncate font-mono text-xs" title={r.route}>
                      {r.route}
                    </Td>
                    <Td numeric>{formatCount(r.total, locale)}</Td>
                    <Td numeric className="text-xs">
                      <span className="text-success">{formatCount(r.s2xx, locale)}</span>
                      {' / '}
                      <span className="text-warning">{formatCount(r.s4xx, locale)}</span>
                      {' / '}
                      <span className={r.s5xx > 0 ? 'font-medium text-destructive' : 'text-muted-foreground'}>
                        {formatCount(r.s5xx, locale)}
                      </span>
                    </Td>
                    <Td numeric>{formatMs(r.latency_ms.avg, locale)}</Td>
                    <Td numeric>{formatMs(r.latency_ms.p50, locale)}</Td>
                    <Td numeric>{formatMs(r.latency_ms.p95, locale)}</Td>
                    <Td numeric>{formatMs(r.latency_ms.p99, locale)}</Td>
                  </Tr>
                ))}
              </TBody>
            </Table>
          )}
        </CardContent>
      </Card>

      <div className="grid gap-3 lg:grid-cols-2">
        <Card>
          <CardHeader>
            <CardTitle className="text-base">{t('monitor:metricsPoolQualityTitle')}</CardTitle>
          </CardHeader>
          <CardContent>
            {d.pool_quality_events.length === 0 ? (
              <p className="text-sm text-muted-foreground">{t('monitor:metricsNoPoolQuality')}</p>
            ) : (
              <Table dense className="tabular-nums" aria-label={t('monitor:metricsPoolQualityTitle')}>
                <THead>
                  <Tr>
                    <Th>{t('monitor:metricsPool')}</Th>
                    <Th>{t('monitor:metricsAction')}</Th>
                    <Th numeric>{t('monitor:metricsCount')}</Th>
                  </Tr>
                </THead>
                <TBody>
                  {d.pool_quality_events.map((ev) => (
                    <Tr key={`${ev.pool}:${ev.action}`}>
                      <Td className="font-mono text-xs">{ev.pool}</Td>
                      <Td>
                        <Badge variant={ev.action.includes('eject') ? 'destructive' : 'warning'}>{ev.action}</Badge>
                      </Td>
                      <Td numeric>{formatCount(ev.count, locale)}</Td>
                    </Tr>
                  ))}
                </TBody>
              </Table>
            )}
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle className="text-base">{t('monitor:metricsWorkerTitle')}</CardTitle>
          </CardHeader>
          <CardContent>
            {d.worker_jobs.length === 0 ? (
              <p className="text-sm text-muted-foreground">{t('monitor:metricsNoWorker')}</p>
            ) : (
              <Table dense className="tabular-nums" aria-label={t('monitor:metricsWorkerTitle')}>
                <THead>
                  <Tr>
                    <Th>{t('monitor:metricsJob')}</Th>
                    <Th numeric>{t('monitor:metricsCount')}</Th>
                    <Th>{t('monitor:metricsOutcomes')}</Th>
                  </Tr>
                </THead>
                <TBody>
                  {d.worker_jobs.map((j) => (
                    <Tr key={j.job}>
                      <Td className="font-mono text-xs">{j.job}</Td>
                      <Td numeric>{formatCount(j.total, locale)}</Td>
                      <Td className="text-xs text-muted-foreground">
                        {Object.entries(j.outcomes)
                          .map(([k, v]) => `${k}:${formatCount(v, locale)}`)
                          .join(' · ')}
                      </Td>
                    </Tr>
                  ))}
                </TBody>
              </Table>
            )}
          </CardContent>
        </Card>
      </div>

      <Card>
        <CardHeader>
          <CardTitle className="text-base">{t('monitor:metricsNodesTitle')}</CardTitle>
        </CardHeader>
        <CardContent>
          <Table dense className="tabular-nums" aria-label={t('monitor:metricsNodesTitle')}>
            <THead>
              <Tr>
                <Th>{t('monitor:metricsNode')}</Th>
                <Th>{t('monitor:metricsRole')}</Th>
                <Th numeric>RPS</Th>
                <Th numeric>{t('monitor:metricsInFlight')}</Th>
                <Th numeric>{t('monitor:metricsPgPool')}</Th>
                <Th numeric>{t('monitor:metricsPricebook')}</Th>
                <Th numeric>{t('monitor:metricsSettlement')}</Th>
                <Th numeric>{t('monitor:metricsCaches')}</Th>
              </Tr>
            </THead>
            <TBody>
              {d.nodes.map((n) => (
                <Tr key={`${n.node}:${n.role}`}>
                  <Td className="font-mono text-xs">{n.node}</Td>
                  <Td>
                    <Badge variant="muted">{n.role}</Badge>
                  </Td>
                  <Td numeric>{formatCount(n.gateway_rps + n.console_rps, locale)}</Td>
                  <Td numeric>{formatCount(n.in_flight, locale)}</Td>
                  <Td numeric className="text-xs">
                    {n.runtime.pg_pool_active} / {n.runtime.pg_pool_size}
                  </Td>
                  <Td numeric>#{n.runtime.pricebook_epoch}</Td>
                  <Td numeric>
                    {n.runtime.settle_backlog}
                    {n.runtime.settle_shedding && (
                      <Badge variant="destructive" className="ml-1.5">
                        {t('monitor:metricsShedding')}
                      </Badge>
                    )}
                  </Td>
                  <Td numeric className="text-xs text-muted-foreground">
                    {n.runtime.caches.models} / {n.runtime.caches.candidates} / {n.runtime.caches.pool_policies}
                  </Td>
                </Tr>
              ))}
            </TBody>
          </Table>
        </CardContent>
      </Card>
    </div>
  )
}
