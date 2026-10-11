import { useTranslation } from 'react-i18next'
import { Link } from '@tanstack/react-router'
import { ArrowUpRight } from 'lucide-react'
import type { PortalLogSearch } from '@/features/logs/search'
import { Button } from '@/components/ui/button'
import { Card } from '@/components/ui/card'
import { OTHER_COLOR } from '@/lib/chart'
import { formatBp } from '@/lib/money'
import { SpendTrendView } from './SpendTrendView'
import type { PortalView } from './search'
import type { BreakdownResp } from './types'
import { sumByModel } from './types'
import { formatUsd } from './format'
import { modelColors } from './usage-chart-data'
import type { UsageMetric } from './usage-chart-data'

/// 总览页签只回答「钱花在哪」：趋势 + 模型排行。Token 构成在自己的页签里，复用同一份用量响应，切换不额外查询。
export function UsageOverview({ data, logSearch, metric, onMetricChange, onView }: {
  data: BreakdownResp; logSearch: PortalLogSearch; metric: UsageMetric
  onMetricChange: (metric: UsageMetric) => void; onView: (view: PortalView) => void
}) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const colors = modelColors(data.data)
  const models = [...sumByModel(data.data).values()].sort((a, b) => b.amount_micro - a.amount_micro || b.requests - a.requests || a.model.localeCompare(b.model))
  const shown = models.slice(0, 5)
  const rest = models.slice(5).reduce((acc, m) => acc + m.amount_micro, 0)
  const sum = data.total.amount_micro
  const share = (micro: number) => sum > 0 ? formatBp(Math.round(Math.min(1, micro / sum) * 10_000), locale) : '—'
  // 环形图：前 5 个模型各一段（颜色与趋势图一致），其余合并成灰色一段
  let offset = 0
  const slices = sum > 0 ? [...shown.map((m) => [m.amount_micro, colors.get(m.model) ?? OTHER_COLOR] as const), [rest, OTHER_COLOR] as const]
    .filter(([micro]) => micro > 0)
    .map(([micro, color]) => {
      const start = offset
      offset += micro
      return `${color} ${start / sum * 100}% ${Math.min(100, offset / sum * 100)}%`
    }) : []

  return <div className="grid min-w-0 gap-3 lg:grid-cols-12">
    <div className="min-w-0 lg:col-span-8"><SpendTrendView quick rows={data.data} days={data.days} window={data.window} metric={metric} onMetricChange={onMetricChange} onExpand={() => onView('trend')} /></div>
    <Card className="flex min-w-0 flex-col rounded-xl px-4 py-3 lg:col-span-4">
      <section aria-label={t('portal:modelSnapshot')} className="flex flex-1 flex-col gap-3">
        <div className="flex items-center justify-between gap-2">
          <h2 className="text-sm font-semibold">{t('portal:modelSnapshot')}</h2>
          {models.length > 0 && <Button variant="ghost" size="xs" onClick={() => onView('models')}>{t('portal:allModels', { count: models.length })}</Button>}
        </div>
        {models.length === 0 ? <p className="py-2 text-xs text-muted-foreground">{t('portal:emptyUsageHint')}</p> : <>
          <div role="img" aria-label={t('portal:modelShareChart')} data-slot="model-donut" className="relative mx-auto size-32 shrink-0 rounded-full bg-muted sm:size-36" style={{ background: slices.length ? `conic-gradient(${slices.join(', ')})` : undefined }}>
            <div aria-hidden className="absolute inset-[16%] flex flex-col items-center justify-center rounded-full bg-card px-2">
              <span className="text-base font-semibold tabular-nums">{formatUsd(data.total.amount_micro, locale)}</span>
              <span className="text-[11px] text-muted-foreground">{t('portal:totalSpend')}</span>
            </div>
          </div>
          <ol className="space-y-2">
            {shown.map((model) => <li key={model.model} className="flex min-w-0 items-center justify-between gap-2 text-sm">
              <Link to="/portal/logs" search={{ ...logSearch, model: model.model }} aria-label={t('portal:modelLogs', { model: model.model })} className="group flex min-w-0 items-center gap-2 rounded outline-none hover:underline focus-visible:ring-2 focus-visible:ring-primary/40">
                <span aria-hidden className="h-2 w-2 shrink-0 rounded-full" style={{ background: colors.get(model.model) ?? OTHER_COLOR }} />
                <span className="truncate" title={model.model}>{model.model}</span>
                <ArrowUpRight aria-hidden className="h-3 w-3 shrink-0 text-muted-foreground opacity-0 group-hover:opacity-100" />
              </Link>
              <span className="flex shrink-0 items-baseline gap-2 tabular-nums">
                {formatUsd(model.amount_micro, locale)}
                <span className="w-12 text-right text-xs text-muted-foreground">{share(model.amount_micro)}</span>
              </span>
            </li>)}
            {rest > 0 && <li className="flex items-center justify-between gap-2 text-xs text-muted-foreground">
              <span className="flex items-center gap-2"><span aria-hidden className="h-2 w-2 shrink-0 rounded-full" style={{ background: OTHER_COLOR }} />{t('portal:otherModels', { count: models.length - shown.length })}</span>
              <span className="flex items-baseline gap-2 tabular-nums">{formatUsd(rest, locale)}<span className="w-12 text-right">{share(rest)}</span></span>
            </li>}
          </ol>
        </>}
      </section>
    </Card>
  </div>
}
