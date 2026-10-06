import { keepPreviousData, useQuery } from '@tanstack/react-query'
import { Activity, Coins, Cpu, LineChart } from 'lucide-react'
import type { LucideIcon } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Segmented } from '@/components/ui/segmented'
import { Skeleton } from '@/components/ui/skeleton'
import { ErrorState } from '@/components/ui/state'
import { TimeChart } from '@/components/ui/time-chart'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatCount, formatMoney } from '@/lib/money'
import { qk } from '@/lib/query-keys'
import { cn } from '@/lib/utils'

type Metric = 'tokens' | 'requests' | 'amount'
type Range = 7 | 30

interface SeriesDay {
  day: string
  requests: number
  errors: number
  tokens: number
  amount_micro: number
}

interface KeySeries {
  window: { start_date: string; end_date: string; timezone: string }
  data: SeriesDay[]
}

/// 后端旧版本 / 异常响应不能被当成"全是零"画出一条平线——形状不对就视为不可用。
function isSeries(value: unknown): value is KeySeries {
  const series = value as KeySeries | null
  return !!series && Array.isArray(series.data) && series.data.length > 0 && typeof series.window?.timezone === 'string'
    && series.data.every((row) => /^\d{4}-\d{2}-\d{2}$/.test(row.day) && [row.requests, row.tokens, row.amount_micro].every((n) => Number.isFinite(n)))
}

const METRICS: Array<{ key: Metric; icon: LucideIcon; color: string }> = [
  { key: 'tokens', icon: Cpu, color: 'var(--color-chart-4)' },
  { key: 'requests', icon: Activity, color: 'var(--color-chart-1)' },
  { key: 'amount', icon: Coins, color: 'var(--color-success)' },
]

/// 密钥用量折线图：近 7 / 30 天逐日走势。三个指标（Token / 请求 / 实际消费）既是合计读数也是切换页签。
///
/// 数据来自 `/api/me/logs/series`——与列表里的迷你折线、下方汇总同一个 PG 账本与同一套过滤；
/// 没有调用的日子由服务端补零，所以这里的"零"是真零，加载失败则明确报"暂不可用"而不是画平线。
export function KeyUsageSeries({ keyId, name }: { keyId: number; name: string }) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const [days, setDays] = useState<Range>(7)
  const [metric, setMetric] = useState<Metric>('tokens')
  const query = useQuery({
    queryKey: qk.keyUsageSeries(keyId, days),
    // scope=user：不限于登录 key，才能查名下的其他密钥；归属仍由后端按登录用户收口
    queryFn: () => apiFetch<unknown>(`/api/me/logs/series?scope=user&api_key_id=${keyId}&days=${days}`),
    staleTime: 30_000,
    retry: false,
    placeholderData: keepPreviousData,
  })
  const series = isSeries(query.data) ? query.data : null
  const usable = series !== null && !query.isError
  const total = (field: (row: SeriesDay) => number) => series?.data.reduce((sum, row) => sum + field(row), 0) ?? 0
  const value = (row: SeriesDay, which: Metric) => (which === 'tokens' ? row.tokens : which === 'requests' ? row.requests : row.amount_micro)
  const show = (n: number, which: Metric) => (which === 'amount' ? formatMoney(n, locale) : formatCount(Math.round(n), locale))
  const labels: Record<Metric, string> = { tokens: t('portal:keySeriesTokens'), requests: t('portal:keySeriesRequests'), amount: t('portal:keySeriesAmount') }
  // 图上的单位：金额轴是美元数值，不是 micro
  const units: Record<Metric, string> = { tokens: labels.tokens, requests: t('portal:keySeriesRequestsUnit'), amount: 'USD' }
  const active = METRICS.find((m) => m.key === metric) ?? METRICS[0]
  const everything = series ? total((r) => r.tokens) + total((r) => r.requests) + total((r) => r.amount_micro) : 0
  const peak = series?.data.reduce<SeriesDay | null>((best, row) => (value(row, metric) > (best ? value(best, metric) : 0) ? row : best), null) ?? null

  return (
    <section data-slot="key-usage-series" aria-label={t('portal:keySeriesTitle')} className="overflow-hidden rounded-xl border border-border bg-card shadow-xs">
      {/* 用 div 而非 header：既有用例按 `dialog header` 取抽屉自己的关闭按钮 */}
      <div className="flex flex-wrap items-center justify-between gap-2 border-b border-border/70 bg-muted/35 px-4 py-2.5">
        <h3 className="flex items-center gap-2 text-sm font-semibold">
          <span aria-hidden className="flex h-6 w-6 items-center justify-center rounded-md bg-primary/10 text-primary"><LineChart className="h-3.5 w-3.5" /></span>
          {t('portal:keySeriesTitle')}
        </h3>
        <Segmented<Range>
          size="sm"
          ariaLabel={t('portal:keySeriesRange')}
          value={days}
          onChange={setDays}
          options={[{ value: 7, label: t('portal:keySeriesDays', { n: 7 }) }, { value: 30, label: t('portal:keySeriesDays', { n: 30 }) }]}
        />
      </div>
      <div className="space-y-3 p-4" aria-busy={query.isFetching}>
        <div role="group" aria-label={t('portal:keySeriesMetric')} className="grid grid-cols-3 gap-2">
          {METRICS.map(({ key, icon: Icon, color }) => {
            const selected = key === metric
            return (
              <button
                key={key}
                type="button"
                aria-pressed={selected}
                onClick={() => setMetric(key)}
                className={cn(
                  'min-w-0 rounded-lg border px-3 py-2 text-left outline-none transition-colors focus-visible:ring-2 focus-visible:ring-primary/40',
                  selected ? 'border-primary/40 bg-primary/5' : 'border-border bg-background hover:border-muted-foreground/40 hover:bg-muted/40',
                )}
              >
                <span className="flex items-center gap-1.5 text-xs text-muted-foreground">
                  <Icon aria-hidden className="h-3.5 w-3.5 shrink-0" style={{ color }} />
                  <span className="truncate">{labels[key]}</span>
                </span>
                <span className="mt-0.5 block truncate text-sm font-semibold tabular-nums sm:text-base" data-slot="key-series-total" title={usable ? show(total((r) => value(r, key)), key) : undefined}>
                  {usable ? show(total((r) => value(r, key)), key) : query.isPending ? <Skeleton className="mt-1 inline-block h-5 w-16 align-middle" /> : '—'}
                </span>
              </button>
            )
          })}
        </div>

        {query.isError && !series ? (
          <ErrorState message={`${t('portal:keySeriesError')} ${describeError(query.error)}`} onRetry={() => void query.refetch()} />
        ) : !series ? (
          query.isPending ? <Skeleton className="h-44 w-full lg:h-56" /> : <ErrorState message={t('portal:keySeriesError')} onRetry={() => void query.refetch()} />
        ) : (
          <>
            <TimeChart
              compact
              controls={false}
              label={t('portal:keySeriesChart', { name })}
              unit={units[metric]}
              format={(n) => show(metric === 'amount' ? n * 1_000_000 : n, metric)}
              series={[{ key: 'v', label: labels[metric], color: active.color }]}
              data={series.data.map((row) => ({ bucket: row.day, v: metric === 'amount' ? row.amount_micro / 1_000_000 : value(row, metric) }))}
            />
            {everything === 0 ? (
              <p role="status" className="rounded-lg bg-muted/60 px-3 py-2 text-xs text-muted-foreground">{t('portal:keySeriesEmpty', { n: days })}</p>
            ) : (
              <dl className="flex flex-wrap gap-x-5 gap-y-1 text-xs text-muted-foreground">
                <div className="flex gap-1.5"><dt>{t('portal:keySeriesAvg')}</dt><dd className="font-medium tabular-nums text-foreground">{show(total((r) => value(r, metric)) / series.data.length, metric)}</dd></div>
                {peak && <div className="flex gap-1.5"><dt>{t('portal:keySeriesPeak')}</dt><dd className="font-medium tabular-nums text-foreground">{show(value(peak, metric), metric)} · {peak.day.slice(5)}</dd></div>}
              </dl>
            )}
            <p className="text-[11px] leading-4 text-muted-foreground">{t('portal:keySeriesNote', { timezone: series.window.timezone })}</p>
          </>
        )}
      </div>
    </section>
  )
}
