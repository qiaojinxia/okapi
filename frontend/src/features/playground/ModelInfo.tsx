import { Link } from '@tanstack/react-router'
import { ArrowUpRight } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { formatContextWindow, modelCapabilities, modelPrice, modelVendor, nonnegative } from '@/features/public-pricing/catalog-data'
import type { PricingModel } from '@/features/public-pricing/types'
import { VendorIcon } from '@/features/public-pricing/VendorIcon'
import { Badge } from '@/components/ui/badge'
import { formatUnitPrice } from '@/lib/money'

/// 模型输入框下的信息卡：厂商、本分组价目、上下文与能力——选之前就知道这次大概花多少。
/// 价目与模型广场同一口径（`modelPrice`）；目录里没有的模型（手输 / 目录缓存落后）不显示卡片。
export function ModelInfo({ model, factor }: { model: PricingModel; factor: number | null }) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const vendor = modelVendor(model)
  const context = nonnegative(model.context_window)
  const price = (field: 'input' | 'output') => modelPrice(model, field, factor)
  const perCall = model.mode === 'per_call' ? modelPrice(model, 'call', factor) : null
  const caps = modelCapabilities(model)
  const cells: Array<{ label: string; value: string }> = model.mode === 'ratio'
    ? [
        { label: `${t('pricing:promptPrice')} / 1M`, value: formatUnitPrice(price('input'), locale) },
        { label: `${t('pricing:completionPrice')} / 1M`, value: formatUnitPrice(price('output'), locale) },
      ]
    : model.mode === 'per_call'
      ? [{ label: t('catalog:billingMode'), value: perCall === null ? '—' : `${formatUnitPrice(perCall, locale)} ${t('catalog:perRequest')}` }]
      : [{ label: t('catalog:billingMode'), value: t('catalog:variablePrice') }]
  if (context !== null && context > 0) cells.push({ label: t('catalog:context'), value: formatContextWindow(context) })
  return (
    <div className="flex flex-col gap-2.5 rounded-lg border border-border bg-muted/30 p-3" data-slot="playground-model-info">
      <div className="flex items-center gap-2.5">
        <VendorIcon vendor={vendor} size="sm" />
        <div className="min-w-0 flex-1">
          <p className="truncate text-sm font-medium leading-5" title={model.display_name || model.model}>{model.display_name || model.model}</p>
          <p className="truncate text-xs text-muted-foreground">{vendor.name || t('catalog:otherVendor')}</p>
        </div>
        <Link to="/pricing" search={{ model: model.model }} aria-label={t('portal:playgroundModelDetails', { model: model.display_name || model.model })}
          className="inline-flex shrink-0 items-center gap-0.5 rounded-md px-1 text-xs text-primary outline-none hover:underline focus-visible:ring-2 focus-visible:ring-primary/40">
          {t('portal:playgroundViewModel')}<ArrowUpRight aria-hidden className="h-3 w-3" />
        </Link>
      </div>
      <dl className="grid grid-cols-3 gap-x-2 gap-y-1 text-xs">
        {cells.map(({ label, value }) => (
          <div key={label} className="min-w-0">
            <dt className="truncate text-muted-foreground">{label}</dt>
            <dd className="truncate font-medium tabular-nums" title={value}>{value}</dd>
          </div>
        ))}
      </dl>
      {caps.length > 0 && (
        <div className="flex flex-wrap gap-1">
          {caps.slice(0, 6).map((cap) => <Badge key={cap} variant="muted">{t(`catalog:cap_${cap}`)}</Badge>)}
        </div>
      )}
    </div>
  )
}
