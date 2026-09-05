import { ArrowRight, ChevronRight, Terminal } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import type { PricingGroup, PricingModel, TokenUnit } from './types'
import { isAvailable, modelCapabilities, modelPrice, modelVendor } from './catalog-data'
import { VendorIcon } from './VendorIcon'
import { Badge } from '@/components/ui/badge'
import { CopyButton } from '@/components/ui/copy-button'
import { Td, Tr } from '@/components/ui/table'
import { formatCount, formatUnitPrice } from '@/lib/money'

export interface ModelItemProps {
  model: PricingModel
  groups: PricingGroup[]
  group: string
  factor: number | null
  unit: TokenUnit
  onOpen: () => void
  onExamples: () => void
}

export function ModelIdentity({ model }: { model: PricingModel }) {
  const { t } = useTranslation()
  const vendor = modelVendor(model)
  return <div className="flex min-w-0 items-start gap-3">
    <VendorIcon vendor={vendor} />
    <div className="min-w-0 flex-1">
      <span className="block truncate text-xs text-muted-foreground">{vendor.name || t('catalog:otherVendor')}</span>
      <h3 title={model.display_name || model.model} className="mt-1 truncate text-base font-semibold tracking-tight">{model.display_name || model.model}</h3>
    </div>
  </div>
}

export function ModelId({ model }: { model: PricingModel }) {
  const { t } = useTranslation()
  return <div className="flex min-w-0 items-center gap-1">
    <code className="min-w-0 truncate text-xs text-muted-foreground" title={model.model}>{model.model}</code>
    <CopyButton value={model.model} label={t('catalog:copyModel')} size="xs" />
  </div>
}

export function ModelAvailability({ model, group }: Pick<ModelItemProps, 'model' | 'group'>) {
  const { t } = useTranslation()
  const available = isAvailable(model, group)
  return <Badge variant={available ? 'muted' : 'warning'}>
    {available ? t('catalog:connected') : group ? t('pricing:notInGroup') : t('catalog:notConnected')}
  </Badge>
}

export function ModelTags({ model }: { model: PricingModel }) {
  const { t, i18n } = useTranslation()
  const caps = modelCapabilities(model)
  return <div className="flex min-h-6 flex-wrap items-center gap-1.5">
    {model.context_window != null && model.context_window > 0 && <Badge variant="outline" title={t('catalog:contextExact', { n: model.context_window.toLocaleString(i18n.language) })}>
      {formatCount(model.context_window, i18n.language)} {t('catalog:contextShort')}
    </Badge>}
    {caps.slice(0, 3).map((cap) => <Badge key={cap} variant="muted">{t(`catalog:cap_${cap}`)}</Badge>)}
    {caps.length > 3 && <Badge variant="muted" title={caps.slice(3).map((cap) => t(`catalog:cap_${cap}`)).join(' · ')}>+{caps.length - 3}</Badge>}
    {caps.length === 0 && !(model.context_window && model.context_window > 0) && <span className="text-xs text-muted-foreground">{t(`analysis:${model.mode}`, { defaultValue: t('catalog:customPricing') })}</span>}
  </div>
}

export function ModelPriceSummary({ model, factor, unit }: Pick<ModelItemProps, 'model' | 'factor' | 'unit'>) {
  const { t, i18n } = useTranslation()
  if (model.mode !== 'ratio') return <div className="flex h-[76px] flex-col justify-center gap-1 rounded-lg bg-muted/45 px-3">
    <span className="text-xs text-muted-foreground">{t(`analysis:${model.mode}`, { defaultValue: t('catalog:customPricing') })}</span>
    <strong className="text-lg font-semibold tracking-tight">{model.mode === 'per_call'
      ? <>{formatUnitPrice(modelPrice(model, 'call', factor), i18n.language)} <span className="text-xs font-normal text-muted-foreground">{t('catalog:perRequest')}</span></>
      : t('catalog:variablePrice')}</strong>
  </div>
  return <div className="grid h-[76px] grid-cols-2 divide-x divide-border rounded-lg bg-muted/45 py-3">
    {(['input', 'output'] as const).map((field) => <div key={field} className="min-w-0 px-3">
      <div className="flex items-center justify-between gap-1 text-xs text-muted-foreground"><span>{t(field === 'input' ? 'pricing:promptPrice' : 'pricing:completionPrice')}</span><span>/ {unit}</span></div>
      <strong className="mt-1 block truncate text-lg font-semibold tracking-tight" title={formatUnitPrice(modelPrice(model, field, factor, unit), i18n.language)}>{formatUnitPrice(modelPrice(model, field, factor, unit), i18n.language)}</strong>
    </div>)}
  </div>
}

export function ModelCard(props: ModelItemProps) {
  const { model, group, groups, onOpen, onExamples } = props
  const { t } = useTranslation()
  const names = model.groups.map((code) => groups.find((g) => g.code === code)?.name || (code === 'default' ? t('flow:defaultGroup') : code))
  return <article data-model={model.model} className="flex min-w-0 flex-col gap-3 rounded-xl border border-border bg-card p-5 shadow-card transition-[border-color,box-shadow] hover:border-primary/40 hover:shadow-popover">
    <button type="button" className="min-w-0 rounded-md text-left outline-none focus-visible:ring-2 focus-visible:ring-primary/40" onClick={onOpen} aria-label={t('catalog:openModel', { model: model.display_name || model.model })}>
      <ModelIdentity model={model} />
    </button>
    <ModelId model={model} />
    <ModelTags model={model} />
    <div className="mt-auto"><ModelPriceSummary {...props} /></div>
    <div className="flex min-w-0 items-center gap-2">
      <ModelAvailability model={model} group={group} />
      <span className="truncate text-xs text-muted-foreground" title={names.join(' · ')}>{names.slice(0, 2).join(' · ')}{names.length > 2 ? ` +${names.length - 2}` : ''}</span>
    </div>
    <div className="-mx-1 -mb-1 flex items-center justify-between gap-2"><button type="button" onClick={onOpen} className="flex min-h-9 items-center gap-2 rounded-md px-1 text-xs font-medium text-primary outline-none hover:bg-primary/5 focus-visible:ring-2 focus-visible:ring-primary/40">
      {t('catalog:details')}<ArrowRight className="h-4 w-4" />
    </button>
    <button type="button" onClick={onExamples} className="flex min-h-9 items-center gap-1.5 rounded-md px-2 text-xs font-medium outline-none hover:bg-muted focus-visible:ring-2 focus-visible:ring-primary/40"><Terminal className="h-3.5 w-3.5" />{t('catalog:examples')}</button></div>
  </article>
}

export function ModelTableRow(props: ModelItemProps) {
  const { model, group, factor, unit, onOpen, onExamples } = props
  const { t, i18n } = useTranslation()
  return <Tr data-model={model.model}>
    <Td className="min-w-64 max-w-96"><button type="button" onClick={onOpen} className="w-full rounded-md text-left outline-none focus-visible:ring-2 focus-visible:ring-primary/40" aria-label={t('catalog:openModel', { model: model.display_name || model.model })}><ModelIdentity model={model} /></button><div className="mt-2"><ModelId model={model} /></div></Td>
    <Td><ModelTags model={model} /></Td>
    <Td><span className="text-xs text-muted-foreground">{t(`analysis:${model.mode}`, { defaultValue: t('catalog:customPricing') })}</span></Td>
    {model.mode === 'ratio' ? <><Td numeric>{formatUnitPrice(modelPrice(model, 'input', factor, unit), i18n.language)}</Td><Td numeric>{formatUnitPrice(modelPrice(model, 'output', factor, unit), i18n.language)}</Td></>
      : <Td colSpan={2} className="text-center">{model.mode === 'per_call' ? `${formatUnitPrice(modelPrice(model, 'call', factor), i18n.language)} ${t('catalog:perRequest')}` : t('catalog:variablePrice')}</Td>}
    <Td><ModelAvailability model={model} group={group} /></Td>
    <Td><div className="flex items-center gap-1"><button type="button" onClick={onExamples} aria-label={t('catalog:examples')} title={t('catalog:examples')} className="flex h-9 w-9 items-center justify-center rounded-lg outline-none hover:bg-muted focus-visible:ring-2 focus-visible:ring-primary/40"><Terminal className="h-4 w-4" /></button><button type="button" onClick={onOpen} aria-label={t('catalog:openModel', { model: model.display_name || model.model })} className="flex h-9 w-9 items-center justify-center rounded-lg text-primary outline-none hover:bg-primary/10 focus-visible:ring-2 focus-visible:ring-primary/40"><ChevronRight className="h-4 w-4" /></button></div></Td>
  </Tr>
}
