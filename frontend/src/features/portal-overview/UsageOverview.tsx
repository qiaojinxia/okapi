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

/// 环形图里的前几名；展开后列表给出全部模型。
const TOP = 5
/// 「其余模型」那一段的键（不会与模型名冲突）。
const OTHER = '\u0000other'

/// 趋势 + 模型消费排行：回答「钱花在哪、怎么变的」。
/// 排行每行除金额和占比外，再给请求数、Token、每次均价和缓存命中——原先单独一张明细表的内容并在这里。
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
  const ratio = (micro: number) => sum > 0 ? Math.min(1, micro / sum) : 0
  // 环形图：前 5 个模型各一段（颜色与趋势图一致），其余合并成灰色一段
  const slices = sum > 0 ? [...top.map((m) => ({ key: m.model, micro: m.amount_micro, color: colors.get(m.model) ?? OTHER_COLOR })), { key: OTHER, micro: rest, color: OTHER_COLOR }]
    .filter((slice) => slice.micro > 0) : []
  // 悬停排行某一行时环形图突出对应一段，其余变淡
  const [active, setActive] = useState<string | null>(null)
  const hover = (key: string) => ({ onMouseEnter: () => setActive(key), onMouseLeave: () => setActive(null), onFocus: () => setActive(key), onBlur: () => setActive(null) })
  // 悬停的行在环上对应哪一段：有金额的前几名是自己那段，展开后榜外的并在「其余」那段；
  // 金额为 0 的行环上没有段，环就不动——否则每一段都被判成「别人」，整圈一起变暗
  const activeSlice = active == null ? null
    : slices.some((slice) => slice.key === active) ? active
      : models.some((m) => m.model === active && m.amount_micro > 0) && slices.some((slice) => slice.key === OTHER) ? OTHER : null

  // 大屏下这一行吃掉页面剩余高度：两张卡同高，趋势图随之拉高，排行里的环形图在余量里居中放大。
  // 视口放不下时按排行实际要列的行数定下限（每行约 3.4rem，加上标题、圆环最小高度与内边距），
  // 模型少就不白占高度；行数很多时封顶，超出的部分在排行卡内滚动。
  const listedRows = expanded ? TOP : Math.min(models.length, TOP) + (rest > 0 ? 1 : 0)
  const minHeight = `${Math.min(30, Math.max(22, 11.5 + listedRows * 3.4))}rem`
  return <div data-slot="usage-overview" style={{ '--usage-min': minHeight } as React.CSSProperties}
    className={cn('grid min-w-0 gap-3 lg:min-h-[var(--usage-min)] lg:grid-cols-12 lg:grid-rows-[minmax(0,1fr)]', className)}>
    <SpendTrendView className="lg:col-span-8 lg:min-h-0" rows={data.data} days={data.days} window={data.window} metric={metric} onMetricChange={onMetricChange} />
    <Card id="portal-models" className="flex min-w-0 scroll-mt-4 flex-col rounded-2xl border-border/70 px-5 py-3 lg:col-span-4 lg:min-h-0">
      <section aria-label={t('portal:modelSnapshot')} className="@container flex min-h-0 flex-1 flex-col gap-3">
        <div className="flex shrink-0 items-center justify-between gap-2">
          <h2 className="text-[15px] font-semibold tracking-tight">{t('portal:modelSnapshot')}</h2>
          {models.length > TOP && <Button variant="ghost" size="xs" aria-expanded={expanded} onClick={() => setExpanded((v) => !v)}>
            {expanded ? t('portal:showTopModels', { count: TOP }) : t('portal:allModels', { count: models.length })}
          </Button>}
        </div>
        {models.length === 0 ? <p className="py-2 text-xs text-muted-foreground">{t('portal:emptyUsageHint')}</p> : <div className="flex min-h-0 min-w-0 flex-1 flex-col gap-3 @xl:flex-row @xl:items-center @xl:gap-6">
          {/* 卡片够宽（单列布局占满一行）时环形图放左、排行放右，免得下面空一大片；
              大屏列布局时环形图区吃掉卡片的剩余高度，圆环在里面居中并随高度放大（最大 18rem） */}
          <div className="flex shrink-0 items-center justify-center lg:min-h-28 lg:flex-1">
            <div className="relative mx-auto size-28 shrink-0 @xl:mx-2 @xl:size-36 lg:aspect-square lg:h-full lg:max-h-60 lg:min-h-28 lg:w-auto">
              <Donut slices={slices} total={sum} active={activeSlice} label={t('portal:modelShareChart')} />
              <div aria-hidden className="pointer-events-none absolute inset-0 flex flex-col items-center justify-center">
                <span className="text-[10px] text-muted-foreground">{t('portal:totalSpend')}</span>
                <span className={cn('font-semibold leading-tight tabular-nums', formatUsd(sum, locale).length > 9 ? 'text-xs' : 'text-[13px]', '@xl:text-sm lg:text-sm')}>{formatUsd(sum, locale)}</span>
                <span className="text-[10px] text-muted-foreground">{t('portal:modelCount', { count: models.length })}</span>
              </div>
            </div>
          </div>
          <ol className={cn('-mx-2 min-w-0 shrink-0 space-y-0.5 @xl:mx-0 @xl:flex-1 lg:min-h-0 lg:shrink lg:overflow-y-auto', expanded && 'max-h-96 overflow-y-auto lg:max-h-none')}>
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
              return <li key={model.model} className="min-w-0">
                <Link to="/portal/logs" search={{ ...logSearch, model: model.model }} aria-label={t('portal:modelLogs', { model: model.model })} {...hover(model.model)}
                  className={cn('group block min-w-0 rounded-lg px-2 py-1 outline-none transition-[background-color,opacity] hover:bg-muted/60 focus-visible:ring-2 focus-visible:ring-primary/40', dimmed && 'opacity-60')}>
                  <div className="flex min-w-0 items-center gap-2 text-[13px]">
                    <span aria-hidden className="w-4 shrink-0 text-center text-[11px] font-medium tabular-nums text-muted-foreground">{index + 1}</span>
                    <span className="min-w-0 flex-1 truncate font-medium" title={model.model}>{model.model}</span>
                    <span className="shrink-0 font-semibold tabular-nums">{formatUsd(model.amount_micro, locale)}</span>
                    <span className="w-12 shrink-0 text-right text-[11px] tabular-nums text-muted-foreground">{share(model.amount_micro)}</span>
                  </div>
                  <span aria-hidden className="mt-1 ml-6 block h-1 overflow-hidden rounded-full bg-muted">
                    <span className="block h-full rounded-full" style={{ width: `${Math.max(ratio(model.amount_micro) * 100, 1.5)}%`, background: color }} />
                  </span>
                  <p className="mt-0.5 truncate pl-6 text-[11px] tabular-nums text-muted-foreground" title={details}>{details}</p>
                </Link>
              </li>
            })}
            {!expanded && rest > 0 && <li className={cn('min-w-0 rounded-lg px-2 py-1 text-xs text-muted-foreground transition-opacity', active != null && active !== OTHER && 'opacity-60')} {...hover(OTHER)}>
              <div className="flex items-center gap-2">
                <span aria-hidden className="w-4 shrink-0" />
                <span className="min-w-0 flex-1 truncate">{t('portal:otherModels', { count: models.length - top.length })}</span>
                <span className="shrink-0 tabular-nums">{formatUsd(rest, locale)}</span>
                <span className="w-12 shrink-0 text-right text-[11px] tabular-nums">{share(rest)}</span>
              </div>
              <span aria-hidden className="mt-1 ml-6 block h-1 overflow-hidden rounded-full bg-muted">
                <span className="block h-full rounded-full" style={{ width: `${Math.max(ratio(rest) * 100, 1.5)}%`, background: OTHER_COLOR }} />
              </span>
            </li>}
          </ol>
        </div>}
      </section>
    </Card>
  </div>
}

/// 环形图（SVG）：段与段之间留一道细缝，悬停排行时对应一段加粗、其余变淡。
function Donut({ slices, total, active, label }: {
  slices: { key: string; micro: number; color: string }[]; total: number; active: string | null; label: string
}) {
  const radius = 38
  const circumference = 2 * Math.PI * radius
  // 只有一段时不留缝，否则整圈会缺一口
  const gap = slices.length > 1 ? 1.6 : 0
  let offset = 0
  return <svg role="img" aria-label={label} data-slot="model-donut" viewBox="0 0 100 100" className="size-full -rotate-90">
    <circle cx="50" cy="50" r={radius} fill="none" strokeWidth="16" className="stroke-muted" />
    {total > 0 && slices.map((slice) => {
      const length = slice.micro / total * circumference
      const dash = Math.max(length - gap, 0.4)
      const start = offset
      offset += length
      const dimmed = active != null && active !== slice.key
      return <circle key={slice.key} cx="50" cy="50" r={radius} fill="none" stroke={slice.color}
        strokeWidth={active === slice.key ? 19 : 16} strokeDasharray={`${dash} ${circumference - dash}`} strokeDashoffset={-start}
        className="transition-[stroke-width,opacity] duration-200" opacity={dimmed ? 0.35 : 1} />
    })}
  </svg>
}
