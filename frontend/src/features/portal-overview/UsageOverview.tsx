import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Link } from '@tanstack/react-router'
import type { PortalLogSearch } from '@/features/logs/search'
import { Button } from '@/components/ui/button'
import { Card } from '@/components/ui/card'
import { OTHER_COLOR } from '@/lib/chart'
import { formatBp, formatCount } from '@/lib/money'
import { cn } from '@/lib/utils'
import { SpendTrendView } from './SpendTrendView'
import type { BreakdownResp } from './types'
import { sumByModel } from './types'
import { cacheHit } from './cache-metrics'
import { formatUsd } from './format'
import { modelColors } from './usage-chart-data'
import type { UsageMetric } from './usage-chart-data'

/// 排行里先列出的前几名；展开后列出全部模型。
const TOP = 5
/// 「其余模型」那一段的键（不会与模型名冲突）。
const OTHER = '\u0000other'

/// 趋势 + 模型消费排行：回答「钱花在哪、怎么变的」。两张卡等宽并排，窄了上下叠放。
/// 排行先用一条分段条看占比（颜色与趋势图里的模型一致），下面每个模型一行：
/// 名次、名称、金额与占比，再给请求数、Token、每次均价和缓存命中——原先单独一张明细表的内容并在这里。
export function UsageOverview({ data, logSearch, metric, onMetricChange, className }: {
  data: BreakdownResp; logSearch: PortalLogSearch; metric: UsageMetric
  onMetricChange: (metric: UsageMetric) => void
  className?: string
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
  // 分段条：前 5 个模型各一段（颜色与趋势图一致），其余合并成灰色一段；宽度按占比，再小也留 3px
  const slices = sum > 0 ? [...top.map((m) => ({ key: m.model, micro: m.amount_micro, color: colors.get(m.model) ?? OTHER_COLOR })), { key: OTHER, micro: rest, color: OTHER_COLOR }]
    .filter((slice) => slice.micro > 0) : []
  // 悬停排行某一行时分段条突出对应一段，其余变淡
  const [active, setActive] = useState<string | null>(null)
  const hover = (key: string) => ({ onMouseEnter: () => setActive(key), onMouseLeave: () => setActive(null), onFocus: () => setActive(key), onBlur: () => setActive(null) })
  // 悬停的行在条上对应哪一段：有金额的前几名是自己那段，展开后榜外的并在「其余」那段；
  // 金额为 0 的行条上没有段，条就不动——否则每一段都被判成「别人」，整条一起变暗
  const activeSlice = active == null ? null
    : slices.some((slice) => slice.key === active) ? active
      : models.some((m) => m.model === active && m.amount_micro > 0) && slices.some((slice) => slice.key === OTHER) ? OTHER : null

  return <div data-slot="usage-overview" className={cn('grid min-w-0 grid-cols-[repeat(auto-fit,minmax(min(100%,26.25rem),1fr))] items-stretch gap-4', className)}>
    <SpendTrendView rows={data.data} total={data.total} days={data.days} window={data.window} metric={metric} onMetricChange={onMetricChange} />
    <Card id="portal-models" className="flex min-w-0 scroll-mt-4 flex-col rounded-xl border-border px-5 py-5 shadow-none">
      <section aria-label={t('portal:modelSnapshot')} className="flex min-h-0 flex-1 flex-col gap-[18px]">
        <div className="flex flex-wrap items-baseline justify-between gap-x-3 gap-y-1">
          <h2 className="text-[15px] font-semibold">{t('portal:modelSnapshot')}</h2>
          {models.length > 0 && <span className="text-xs text-muted-foreground/80 tabular-nums">{t('portal:modelCount', { count: models.length })} · {t('portal:modelsTotal', { amount: formatUsd(sum, locale) })}</span>}
        </div>
        {models.length === 0 ? <p className="py-2 text-xs text-muted-foreground">{t('portal:emptyUsageHint')}</p> : <>
          <div role="img" aria-label={t('portal:modelShareChart')} data-slot="model-share" className="flex h-2.5 shrink-0 gap-0.5 overflow-hidden rounded-full bg-muted">
            {slices.map((slice) => <span key={slice.key} data-slot="model-share-segment" data-key={slice.key === OTHER ? 'other' : slice.key} className="block min-w-[3px] transition-opacity duration-200"
              style={{ flex: slice.micro / sum, background: slice.color, opacity: activeSlice != null && activeSlice !== slice.key ? 0.35 : 1 }} />)}
          </div>
          <ol className="flex min-w-0 flex-col">
            {listed.map((model, index) => {
              const hit = cacheHit(model)
              const color = colors.get(model.model) ?? OTHER_COLOR
              const details = [
                t('portal:modelRequests', { n: formatCount(model.requests, locale) }),
                t('portal:modelTokens', { n: formatCount(model.prompt_tokens + model.completion_tokens, locale) }),
                model.requests > 0 ? t('portal:modelAvg', { v: formatUsd(Math.round(model.amount_micro / model.requests), locale) }) : null,
                // 只采到部分请求时写明样本数，不把样本命中率当成全量
                hit.bp == null ? null : hit.partial
                  ? t('portal:modelCacheHitMeasured', { v: formatBp(hit.bp, locale), n: hit.samples, total: model.requests })
                  : t('portal:modelCacheHit', { v: formatBp(hit.bp, locale) }),
              ].filter(Boolean).join(' · ')
              const dimmed = active != null && active !== model.model
              return <li key={model.model} className="min-w-0 border-t border-border/60 first:border-t-0">
                <Link to="/portal/logs" search={{ ...logSearch, model: model.model }} aria-label={t('portal:modelLogs', { model: model.model })} {...hover(model.model)}
                  className={cn('group grid min-w-0 grid-cols-[1.25rem_minmax(0,1fr)_auto] gap-x-3 gap-y-1 rounded-md px-1 py-3.5 outline-none transition-[background-color,opacity] hover:bg-muted/50 focus-visible:ring-2 focus-visible:ring-primary/40', dimmed && 'opacity-60')}>
                  <span aria-hidden className="pt-0.5 text-xs tabular-nums text-muted-foreground/70">{index + 1}</span>
                  <span className="flex min-w-0 items-center gap-2">
                    <span aria-hidden className="size-2 shrink-0 rounded-[2px]" style={{ background: color }} />
                    <span className="min-w-0 truncate text-sm font-medium" title={model.model}>{model.model}</span>
                  </span>
                  <span className="flex items-baseline gap-2.5 tabular-nums">
                    <span className="text-sm font-semibold">{formatUsd(model.amount_micro, locale)}</span>
                    <span className="w-12 text-right text-xs text-muted-foreground/80">{share(model.amount_micro)}</span>
                  </span>
                  <span aria-hidden />
                  <span className="col-span-2 min-w-0 truncate text-xs tabular-nums text-muted-foreground/80" title={details}>{details}</span>
                </Link>
              </li>
            })}
            {!expanded && rest > 0 && <li className={cn('min-w-0 border-t border-border/60 transition-opacity', active != null && active !== OTHER && 'opacity-60')} {...hover(OTHER)}>
              <div className="grid grid-cols-[1.25rem_minmax(0,1fr)_auto] gap-x-3 px-1 py-3.5 text-sm text-muted-foreground">
                <span aria-hidden />
                <span className="flex min-w-0 items-center gap-2">
                  <span aria-hidden className="size-2 shrink-0 rounded-[2px]" style={{ background: OTHER_COLOR }} />
                  <span className="min-w-0 truncate">{t('portal:otherModels', { count: models.length - top.length })}</span>
                </span>
                <span className="flex items-baseline gap-2.5 tabular-nums">
                  <span>{formatUsd(rest, locale)}</span>
                  <span className="w-12 text-right text-xs">{share(rest)}</span>
                </span>
              </div>
            </li>}
          </ol>
          {models.length > TOP && <Button variant="ghost" size="xs" className="-ml-1.5 self-start" aria-expanded={expanded} onClick={() => setExpanded((v) => !v)}>
            {expanded ? t('portal:showTopModels', { count: TOP }) : t('portal:allModels', { count: models.length })}
          </Button>}
        </>}
      </section>
    </Card>
  </div>
}
