import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Link } from '@tanstack/react-router'
import { ArrowUpRight } from 'lucide-react'
import type { PortalLogSearch } from '@/features/logs/search'
import { Button } from '@/components/ui/button'
import { Card } from '@/components/ui/card'
import { OTHER_COLOR } from '@/lib/chart'
import { formatBp, formatCount } from '@/lib/money'
import { SpendTrendView } from './SpendTrendView'
import type { BreakdownResp } from './types'
import { sumByModel } from './types'
import { cacheHit } from './cache-metrics'
import { formatUsd } from './format'
import { modelColors } from './usage-chart-data'
import type { UsageMetric } from './usage-chart-data'

/// 环形图里的前几名；展开后列表给出全部模型。
const TOP = 5

/// 趋势 + 模型消费排行：回答「钱花在哪、怎么变的」。
/// 排行每行除金额和占比外，再给请求数、Token、每次均价和缓存命中——原先单独一张明细表的内容并在这里。
export function UsageOverview({ data, logSearch, metric, onMetricChange }: {
  data: BreakdownResp; logSearch: PortalLogSearch; metric: UsageMetric
  onMetricChange: (metric: UsageMetric) => void
}) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const [expanded, setExpanded] = useState(false)
  const colors = modelColors(data.data)
  const models = [...sumByModel(data.data).values()].sort((a, b) => b.amount_micro - a.amount_micro || b.requests - a.requests || a.model.localeCompare(b.model))
  const top = models.slice(0, TOP)
  const rest = models.slice(TOP).reduce((acc, m) => acc + m.amount_micro, 0)
  const listed = expanded ? models : top
  const sum = data.total.amount_micro
  const share = (micro: number) => sum > 0 ? formatBp(Math.round(Math.min(1, micro / sum) * 10_000), locale) : '—'
  // 环形图：前 5 个模型各一段（颜色与趋势图一致），其余合并成灰色一段
  let offset = 0
  const slices = sum > 0 ? [...top.map((m) => [m.amount_micro, colors.get(m.model) ?? OTHER_COLOR] as const), [rest, OTHER_COLOR] as const]
    .filter(([micro]) => micro > 0)
    .map(([micro, color]) => {
      const start = offset
      offset += micro
      return `${color} ${start / sum * 100}% ${Math.min(100, offset / sum * 100)}%`
    }) : []

  return <div className="grid min-w-0 gap-3 lg:grid-cols-12">
    <div className="min-w-0 lg:col-span-8"><SpendTrendView rows={data.data} days={data.days} window={data.window} metric={metric} onMetricChange={onMetricChange} /></div>
    <Card id="portal-models" className="flex min-w-0 scroll-mt-4 flex-col rounded-xl px-4 py-3 lg:col-span-4">
      <section aria-label={t('portal:modelSnapshot')} className="flex min-h-0 flex-1 flex-col gap-3">
        <div className="flex items-center justify-between gap-2">
          <h2 className="text-sm font-semibold">{t('portal:modelSnapshot')}</h2>
          {models.length > TOP && <Button variant="ghost" size="xs" aria-expanded={expanded} onClick={() => setExpanded((v) => !v)}>
            {expanded ? t('portal:showTopModels', { count: TOP }) : t('portal:allModels', { count: models.length })}
          </Button>}
        </div>
        {models.length === 0 ? <p className="py-2 text-xs text-muted-foreground">{t('portal:emptyUsageHint')}</p> : <>
          <div role="img" aria-label={t('portal:modelShareChart')} data-slot="model-donut" className="relative mx-auto size-32 shrink-0 rounded-full bg-muted" style={{ background: slices.length ? `conic-gradient(${slices.join(', ')})` : undefined }}>
            <div aria-hidden className="absolute inset-[16%] flex flex-col items-center justify-center rounded-full bg-card px-2">
              <span className="text-base font-semibold tabular-nums">{formatUsd(sum, locale)}</span>
              <span className="text-[11px] text-muted-foreground">{t('portal:totalSpend')}</span>
            </div>
          </div>
          <ol className={expanded ? 'max-h-96 space-y-2.5 overflow-y-auto pr-1' : 'space-y-2.5'}>
            {listed.map((model) => {
              const hit = cacheHit(model)
              const details = [
                t('portal:modelRequests', { n: formatCount(model.requests, locale) }),
                t('portal:modelTokens', { n: formatCount(model.prompt_tokens + model.completion_tokens, locale) }),
                model.requests > 0 ? t('portal:modelAvg', { v: formatUsd(Math.round(model.amount_micro / model.requests), locale) }) : null,
                // 只采到部分请求时写明样本数，不把样本命中率当成全量
                hit.bp == null ? null : hit.partial
                  ? t('portal:modelCacheHitMeasured', { v: formatBp(hit.bp, locale), n: hit.samples, total: model.requests })
                  : t('portal:modelCacheHit', { v: formatBp(hit.bp, locale) }),
              ].filter(Boolean).join(' · ')
              return <li key={model.model} className="min-w-0 space-y-0.5">
                <div className="flex min-w-0 items-center justify-between gap-2 text-sm">
                  <Link to="/portal/logs" search={{ ...logSearch, model: model.model }} aria-label={t('portal:modelLogs', { model: model.model })} className="group flex min-w-0 items-center gap-2 rounded outline-none hover:underline focus-visible:ring-2 focus-visible:ring-primary/40">
                    <span aria-hidden className="h-2 w-2 shrink-0 rounded-full" style={{ background: colors.get(model.model) ?? OTHER_COLOR }} />
                    <span className="truncate" title={model.model}>{model.model}</span>
                    <ArrowUpRight aria-hidden className="h-3 w-3 shrink-0 text-muted-foreground opacity-0 group-hover:opacity-100" />
                  </Link>
                  <span className="flex shrink-0 items-baseline gap-2 tabular-nums">
                    {formatUsd(model.amount_micro, locale)}
                    <span className="w-12 text-right text-xs text-muted-foreground">{share(model.amount_micro)}</span>
                  </span>
                </div>
                <p className="truncate pl-4 text-[11px] tabular-nums text-muted-foreground" title={details}>{details}</p>
              </li>
            })}
            {!expanded && rest > 0 && <li className="flex items-center justify-between gap-2 text-xs text-muted-foreground">
              <span className="flex items-center gap-2"><span aria-hidden className="h-2 w-2 shrink-0 rounded-full" style={{ background: OTHER_COLOR }} />{t('portal:otherModels', { count: models.length - top.length })}</span>
              <span className="flex items-baseline gap-2 tabular-nums">{formatUsd(rest, locale)}<span className="w-12 text-right">{share(rest)}</span></span>
            </li>}
          </ol>
        </>}
      </section>
    </Card>
  </div>
}
