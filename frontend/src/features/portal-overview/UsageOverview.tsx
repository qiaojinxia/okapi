import { useTranslation } from 'react-i18next'
import { Link } from '@tanstack/react-router'
import { ArrowUpRight } from 'lucide-react'
import type { PortalLogSearch } from '@/features/logs/search'
import { Button } from '@/components/ui/button'
import { Card } from '@/components/ui/card'
import { chartColor } from '@/lib/chart'
import { formatBp, formatCount, formatMoney } from '@/lib/money'
import { SpendTrendView } from './SpendTrendView'
import { segments } from './TokenMixView'
import type { Segment } from './TokenMixView'
import type { PortalView } from './search'
import type { BreakdownResp } from './types'
import { sumByModel } from './types'
import { cacheAmount, cacheHit } from './cache-metrics'
import { CacheTokenValue } from './CacheTokenValue'
import type { UsageMetric } from './usage-chart-data'

// 首页复用同一份用量响应，切换到明细不会额外查询，也不会改变范围或日期。
export function UsageOverview({ data, logSearch, metric, onView }: { data: BreakdownResp; logSearch: PortalLogSearch; metric: UsageMetric; onView: (view: PortalView) => void }) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const models = [...sumByModel(data.data).values()].sort((a, b) => b.amount_micro - a.amount_micro || b.requests - a.requests || a.model.localeCompare(b.model))
  const parts = segments(data.total)
  const hit = cacheHit(data.total)
  // 说明在大屏压成一行（悬停看全文），把高度让给排行；窄屏单列时照常换行
  const notes = [
    data.total.cache_hit_bp == null || data.total.cache_write_tokens == null
      ? t(data.total.cache_hit_bp == null ? 'portal:cacheIncomplete' : 'charts:missingCacheWrite') : null,
    hit.partial ? t('portal:cacheHitMeasured', { v: formatBp(hit.bp, locale), n: hit.samples, total: data.total.requests }) : null,
  ].filter((note): note is string => note !== null)
  const sum = parts.reduce((n, part) => n + part.value, 0)
  const labels: Record<Segment['key'], string> = {
    input: t('portal:tokInput'), cached: t('portal:tokCached'), write: t('charts:cacheWrite'),
    output: t('portal:tokOutput'), reasoning: t('portal:tokReasoning'),
  }
  return <div className="grid min-w-0 gap-3 lg:grid-cols-[minmax(0,1.4fr)_minmax(19rem,1fr)]">
    <SpendTrendView rows={data.data} days={data.days} window={data.window} metric={metric} onExpand={() => onView('trend')} />
    <Card className="min-w-0 divide-y divide-border rounded-xl">
      <section aria-label={t('portal:modelSnapshot')} className="space-y-2 px-4 py-3">
        <div className="flex flex-wrap items-center justify-between gap-x-2">
          <h2 className="text-sm font-semibold">{t('portal:modelSnapshot')}</h2>
          <Button variant="ghost" size="xs" onClick={() => onView('models')}>{t('portal:allModels', { count: models.length })}</Button>
        </div>
        {models.length === 0 ? <p className="py-2 text-xs text-muted-foreground">{t('portal:emptyUsageHint')}</p> : <ol className="space-y-2">
          {models.slice(0, 4).map((model, index) => {
            const share = data.total.amount_micro > 0 ? Math.min(100, model.amount_micro / data.total.amount_micro * 100) : 0
            return <li key={model.model} className="space-y-1">
              <div className="flex min-w-0 items-baseline justify-between gap-2 text-xs">
                <span className="flex min-w-0 items-start gap-2 font-medium"><span className="text-muted-foreground">{index + 1}</span><Link to="/portal/logs" search={{ ...logSearch, model: model.model }} aria-label={t('portal:modelLogs', { model: model.model })} className="group flex min-w-0 items-start gap-1 rounded text-primary outline-none hover:underline focus-visible:ring-2 focus-visible:ring-primary/40"><span className="break-all">{model.model}</span><ArrowUpRight aria-hidden className="mt-0.5 h-3 w-3 shrink-0" /></Link></span>
                <span className="shrink-0 tabular-nums">{formatMoney(model.amount_micro, locale)}</span>
              </div>
              <div className="flex items-center gap-2">
                <div className="h-1.5 min-w-0 flex-1 overflow-hidden rounded-full bg-muted" aria-hidden><div className="h-full rounded-full" style={{ width: `${share}%`, background: chartColor(index) }} /></div>
                <span className="w-12 text-right text-[11px] tabular-nums text-muted-foreground">{data.total.amount_micro > 0 ? formatBp(Math.round(share * 100), locale) : '—'}</span>
              </div>
            </li>
          })}
        </ol>}
      </section>
      <section aria-label={t('portal:tokenSnapshot')} className="space-y-1.5 px-4 py-2.5">
        <div className="flex flex-wrap items-center justify-between gap-x-2">
          <h2 className="text-sm font-semibold">{t('portal:tokenSnapshot')}</h2>
          <Button variant="ghost" size="xs" onClick={() => onView('tokens')}>{t('portal:expandTokens')}</Button>
        </div>
        <div className="flex h-2 overflow-hidden rounded-full bg-muted" aria-hidden>
          {parts.filter((part) => part.value > 0).map((part) => <div key={part.key} className={part.className} style={{ width: `${sum > 0 ? part.value / sum * 100 : 0}%` }} />)}
        </div>
        <dl className="grid grid-cols-2 gap-x-4 gap-y-1 text-xs sm:grid-cols-3 lg:grid-cols-2 2xl:grid-cols-3">
          {parts.map((part) => <div key={part.key} className="flex min-w-0 items-center justify-between gap-2">
            <dt className="flex min-w-0 items-center gap-1.5 text-muted-foreground"><span aria-hidden className={`h-2 w-2 shrink-0 rounded-sm ${part.className}`} />{labels[part.key]}</dt>
            <dd className="shrink-0 tabular-nums">{part.key === 'write' || part.key === 'cached'
              ? <CacheTokenValue value={cacheAmount(data.total, part.key === 'cached' ? 'read' : 'write')} /> : formatCount(part.value, locale)}</dd>
          </div>)}
        </dl>
        {notes.map((note) => <p key={note} title={note} className="text-[11px] leading-4 text-muted-foreground lg:truncate">{note}</p>)}
      </section>
    </Card>
  </div>
}
