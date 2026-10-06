import { ChevronRight } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { cn } from '@/lib/utils'
import type { ModelPreset } from './model-presets'

type Lane = 'input' | 'output' | 'cacheRead' | 'cacheWrite5m' | 'cacheWrite1h'

/// 只用于展示：按当前倍率换算出每条计费线的 USD / 1M，并与官方参考价并排。
/// 账单仍由后端按定点倍率计算，这里的浮点仅影响显示的末位。
function usd(value: number) {
  return `$${value.toLocaleString('en-US', { maximumFractionDigits: 4 })}`
}

export function ModelPriceOverview({ basePriceMicro, axes, independent, cacheApplicable, preset, note }: {
  basePriceMicro: number
  axes: Record<string, string>
  independent: Record<string, string>
  /// 模型声明不支持提示缓存时，缓存线不展示。
  cacheApplicable: boolean
  preset?: ModelPreset | null
  /// 换算依据说明（基准价），随展开内容显示。
  note?: string
}) {
  const { t } = useTranslation()
  const ratio = (raw: string | undefined, fallback: string) => {
    const value = Number((raw ?? '').trim() === '' ? fallback : raw)
    return Number.isFinite(value) ? value : NaN
  }
  const input = (basePriceMicro / 1_000_000) * ratio(axes.model_ratio, '1')
  const write = ratio(axes.cache_write_ratio, '1')
  const ours: Record<Lane, number> = {
    input,
    output: input * ratio(axes.completion_ratio, '1'),
    cacheRead: input * ratio(axes.cache_ratio, '1'),
    // 未单独配置时长倍率时，后端按通用写入倍率计费
    cacheWrite5m: input * ratio(independent.cache_write_5m, String(write)),
    cacheWrite1h: input * ratio(independent.cache_write_1h, String(write)),
  }
  const reference = preset?.referencePrice
  const refInput = reference ? Number(reference.input) : NaN
  const official: Partial<Record<Lane, number>> = reference ? {
    input: refInput,
    output: Number(reference.output),
    cacheRead: preset?.cache ? refInput * Number(preset.cache.read) : undefined,
    cacheWrite5m: preset?.cache?.write5m ? refInput * Number(preset.cache.write5m) : undefined,
    cacheWrite1h: preset?.cache?.write1h ? refInput * Number(preset.cache.write1h) : undefined,
  } : {}
  const lanes: Lane[] = cacheApplicable
    ? ['input', 'output', 'cacheRead', 'cacheWrite5m', 'cacheWrite1h'] : ['input', 'output']
  if (lanes.some((lane) => !Number.isFinite(ours[lane]))) return null
  const max = Math.max(...lanes.map((lane) => Math.max(ours[lane], official[lane] ?? 0)), Number.EPSILON)
  const markup = reference && refInput > 0 ? input / refInput : undefined

  const brief = (['input', 'output', ...(cacheApplicable ? ['cacheRead'] as const : [])] as const)
    .map((lane) => `${t(`admin:modelPriceOverview.${lane}`)} ${usd(ours[lane])}`).join(' · ')

  // 默认一行摘要，保持首屏紧凑；展开看各计费线的条形对比
  return (
    <details className="group/price rounded-lg border border-border px-3 py-2" data-testid="model-price-overview">
      <summary className="flex cursor-pointer list-none flex-wrap items-center gap-x-2 gap-y-1 text-xs [&::-webkit-details-marker]:hidden">
        <ChevronRight aria-hidden className="h-3.5 w-3.5 shrink-0 text-muted-foreground transition-transform group-open/price:rotate-90" />
        <span className="font-medium">{t('admin:modelPriceOverview.title')}</span>
        <span className="min-w-0 flex-1 truncate text-muted-foreground tabular-nums">{brief}</span>
        {markup !== undefined && (
          <span className={cn('rounded px-1.5 py-0.5 text-[11px] tabular-nums',
            Math.abs(markup - 1) < 1e-9 ? 'bg-muted text-muted-foreground'
              : markup < 1 ? 'bg-warning/15 text-warning' : 'bg-success/15 text-success')}>
            {t('admin:modelPriceOverview.markup', { ratio: markup.toLocaleString('en-US', { maximumFractionDigits: 3 }) })}
          </span>
        )}
      </summary>
      <div className="mt-2 grid grid-cols-[6.5rem_minmax(0,1fr)_auto] items-center gap-x-3 gap-y-1.5 text-xs">
        {lanes.map((lane) => {
          const ref = official[lane]
          const differs = ref !== undefined && Math.abs(ours[lane] - ref) > 1e-9
          return (
            <div key={lane} className="contents">
              <span className="text-muted-foreground">{t(`admin:modelPriceOverview.${lane}`)}</span>
              <div className="relative h-1.5 overflow-hidden rounded-full bg-muted" aria-hidden>
                <div className="h-full rounded-full bg-primary/70" style={{ width: `${(ours[lane] / max) * 100}%` }} />
              </div>
              <span className="text-right tabular-nums">
                <span className="font-medium">{usd(ours[lane])}</span>
                {ref !== undefined && (
                  <span className={cn('ml-1.5', differs ? 'text-warning' : 'text-muted-foreground')}
                    title={t('admin:modelPriceOverview.officialTitle')}>
                    {t('admin:modelPriceOverview.official', { price: usd(ref) })}
                  </span>
                )}
              </span>
            </div>
          )
        })}
      </div>
      {note && <p className="mt-2 text-[11px] leading-4 text-muted-foreground">{note}</p>}
      <p className="mt-1 text-[11px] leading-4 text-muted-foreground">{t('admin:modelPriceOverview.hint')}</p>
    </details>
  )
}
