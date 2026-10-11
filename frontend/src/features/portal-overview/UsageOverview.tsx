import { useTranslation } from 'react-i18next'
import { Link } from '@tanstack/react-router'
import { ArrowUpRight } from 'lucide-react'
import type { ReactNode } from 'react'
import type { PortalLogSearch } from '@/features/logs/search'
import { Button } from '@/components/ui/button'
import { Card } from '@/components/ui/card'
import { OTHER_COLOR } from '@/lib/chart'
import { formatBp, formatCount } from '@/lib/money'
import { SpendTrendView } from './SpendTrendView'
import { segments } from './TokenMixView'
import type { Segment } from './TokenMixView'
import type { PortalView } from './search'
import type { BreakdownResp, Scope } from './types'
import { sumByModel } from './types'
import { cacheAmount, cacheHit } from './cache-metrics'
import { CacheTokenValue } from './CacheTokenValue'
import { RecentCalls } from './RecentCalls'
import { formatUsd } from './format'
import { modelColors } from './usage-chart-data'
import type { UsageMetric } from './usage-chart-data'

/// 总览页签：先回答「钱花在哪」（趋势 + 模型），再是「服务好不好」（health），
/// 最后是「Token 怎么构成 / 最近发生了什么」。全部复用同一份用量响应，切换不额外查询。
export function UsageOverview({ data, scope, logSearch, metric, onMetricChange, onView, health }: {
  data: BreakdownResp; scope: Scope; logSearch: PortalLogSearch; metric: UsageMetric
  onMetricChange: (metric: UsageMetric) => void; onView: (view: PortalView) => void; health: ReactNode
}) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const colors = modelColors(data.data)
  const models = [...sumByModel(data.data).values()].sort((a, b) => b.amount_micro - a.amount_micro || b.requests - a.requests || a.model.localeCompare(b.model))
  const parts = segments(data.total)
  const hit = cacheHit(data.total)
  const sum = parts.reduce((n, part) => n + part.value, 0)
  const labels: Record<Segment['key'], string> = {
    input: t('portal:tokInput'), cached: t('portal:tokCached'), write: t('charts:cacheWrite'),
    output: t('portal:tokOutput'), reasoning: t('portal:tokReasoning'),
  }
  const notes = [
    data.total.cache_hit_bp == null || data.total.cache_write_tokens == null
      ? t(data.total.cache_hit_bp == null ? 'portal:cacheIncomplete' : 'charts:missingCacheWrite') : null,
    hit.partial ? t('portal:cacheHitMeasured', { v: formatBp(hit.bp, locale), n: hit.samples, total: data.total.requests }) : null,
  ].filter((note): note is string => note !== null)
  const cachedShare = sum > 0 ? (parts.find((p) => p.key === 'cached')?.value ?? 0) / sum : 0
  const shown = models.slice(0, 5)
  const rest = models.slice(5).reduce((acc, m) => acc + m.amount_micro, 0)

  return <div className="flex min-w-0 flex-col gap-3">
    <div className="grid min-w-0 gap-3 lg:grid-cols-12">
      <div className="min-w-0 lg:col-span-8"><SpendTrendView quick rows={data.data} days={data.days} window={data.window} metric={metric} onMetricChange={onMetricChange} onExpand={() => onView('trend')} /></div>
      <Card className="flex min-w-0 flex-col rounded-xl px-4 py-3 lg:col-span-4">
        <section aria-label={t('portal:modelSnapshot')} className="flex flex-1 flex-col gap-3">
          <div className="flex items-center justify-between gap-2">
            <h2 className="text-sm font-semibold">{t('portal:modelSnapshot')}</h2>
            {models.length > 0 && <Button variant="ghost" size="xs" onClick={() => onView('models')}>{t('portal:allModels', { count: models.length })}</Button>}
          </div>
          {models.length === 0 ? <p className="py-2 text-xs text-muted-foreground">{t('portal:emptyUsageHint')}</p> : <ol className="space-y-3">
            {shown.map((model) => {
              const share = data.total.amount_micro > 0 ? Math.min(100, model.amount_micro / data.total.amount_micro * 100) : 0
              return <li key={model.model} className="space-y-1.5">
                <div className="flex min-w-0 items-baseline justify-between gap-2 text-sm">
                  <Link to="/portal/logs" search={{ ...logSearch, model: model.model }} aria-label={t('portal:modelLogs', { model: model.model })} className="group flex min-w-0 items-center gap-2 rounded outline-none hover:underline focus-visible:ring-2 focus-visible:ring-primary/40">
                    <span aria-hidden className="h-2 w-2 shrink-0 rounded-full" style={{ background: colors.get(model.model) ?? OTHER_COLOR }} />
                    <span className="truncate" title={model.model}>{model.model}</span>
                    <ArrowUpRight aria-hidden className="h-3 w-3 shrink-0 text-muted-foreground opacity-0 group-hover:opacity-100" />
                  </Link>
                  <span className="shrink-0 tabular-nums">{formatUsd(model.amount_micro, locale)}</span>
                </div>
                <div className="flex items-center gap-2">
                  <div className="h-1 min-w-0 flex-1 overflow-hidden rounded-full bg-muted" aria-hidden><div className="h-full rounded-full" style={{ width: `${share}%`, background: colors.get(model.model) ?? OTHER_COLOR }} /></div>
                  <span className="w-12 text-right text-xs tabular-nums text-muted-foreground">{data.total.amount_micro > 0 ? formatBp(Math.round(share * 100), locale) : '—'}</span>
                </div>
              </li>
            })}
            {rest > 0 && <li className="flex items-baseline justify-between text-xs text-muted-foreground"><span>{t('portal:otherModels', { count: models.length - shown.length })}</span><span className="tabular-nums">{formatUsd(rest, locale)}</span></li>}
          </ol>}
        </section>
      </Card>
    </div>

    {health}

    <div className="grid min-w-0 gap-3 lg:grid-cols-12">
      <Card className="min-w-0 rounded-xl px-4 py-3 lg:col-span-8">
        <section aria-label={t('portal:tokenSnapshot')} className="space-y-3">
          <div className="flex flex-wrap items-center justify-between gap-2">
            <h2 className="text-sm font-semibold">{t('portal:tokenSnapshot')}</h2>
            <span className="text-xs tabular-nums text-muted-foreground">{t('portal:tokenTotal', { v: formatCount(data.total.tokens, locale) })}</span>
          </div>
          <div className="flex h-2.5 overflow-hidden rounded-full bg-muted" aria-hidden>
            {parts.filter((part) => part.value > 0).map((part) => <div key={part.key} className={part.className} style={{ width: `${sum > 0 ? part.value / sum * 100 : 0}%` }} />)}
          </div>
          <dl className="grid grid-cols-1 gap-x-6 gap-y-1.5 text-sm sm:grid-cols-2 xl:grid-cols-3">
            {parts.map((part) => <div key={part.key} className="flex min-w-0 items-center justify-between gap-2">
              <dt className="flex min-w-0 items-center gap-1.5 text-muted-foreground"><span aria-hidden className={`h-2 w-2 shrink-0 rounded-sm ${part.className}`} />{labels[part.key]}</dt>
              <dd className="flex shrink-0 items-baseline gap-2 tabular-nums">
                <span>{part.key === 'write' || part.key === 'cached'
                  ? <CacheTokenValue value={cacheAmount(data.total, part.key === 'cached' ? 'read' : 'write')} /> : formatCount(part.value, locale)}</span>
                <span className="w-12 text-right text-xs text-muted-foreground">{sum > 0 ? formatBp(Math.round(part.value / sum * 10_000), locale) : '—'}</span>
              </dd>
            </div>)}
          </dl>
          {/* 缓存读取单价通常只有普通输入的一成左右：按 Token 数看会显得它占了大头，提醒一句 */}
          {cachedShare >= 0.3 && <p className="text-xs leading-5 text-muted-foreground">{t('portal:cacheCheaperNote')}</p>}
          {notes.map((note) => <p key={note} className="text-xs leading-5 text-muted-foreground">{note}</p>)}
          <Button variant="ghost" size="xs" className="-ml-2" onClick={() => onView('tokens')}>{t('portal:expandTokens')}</Button>
        </section>
      </Card>
      <div className="min-w-0 lg:col-span-4"><RecentCalls scope={scope} logSearch={logSearch} /></div>
    </div>
  </div>
}
