import { useTranslation } from 'react-i18next'
import type { PricingGroup, PricingModel, TokenUnit } from './types'
import { modelCapabilities, modelPrice, modelVendor, nonnegative } from './catalog-data'
import type { PriceField } from './catalog-data'
import { ModelAvailability, ModelId, ModelPriceSummary } from './ModelCatalogItem'
import { VendorIcon } from './VendorIcon'
import { Simulator } from './Simulator'
import { RequestExamples } from './RequestExamples'
import { Tabs } from '@/components/ui/tabs'
import { ReceiptText, Terminal } from 'lucide-react'
import { Badge } from '@/components/ui/badge'
import { Drawer, FieldGroup } from '@/components/ui/drawer'
import { Select } from '@/components/ui/select'
import { Label } from '@/components/ui/input'
import { formatRatio, formatUnitPrice } from '@/lib/money'
import { cn } from '@/lib/utils'

export function ModelDetails({ model, groups, group, factor, unit, tab, onTab, onGroup, onClose }: {
  model: PricingModel; groups: PricingGroup[]; group: string; factor: number | null; unit: TokenUnit
  onGroup: (group: string) => void; onClose: () => void
  tab: 'details' | 'code'; onTab: (tab: string) => void
}) {
  const { t, i18n } = useTranslation()
  const vendor = modelVendor(model)
  const caps = modelCapabilities(model)
  const rows: Array<[PriceField, string]> = [
    ['input', t('pricing:promptPrice')], ['output', t('pricing:completionPrice')],
    ['cache', t('pricing:cachedPrice')], ['cacheWrite', t('pricing:cacheWritePrice')],
  ]
  const distinct = (value: string | null) => nonnegative(value) !== null && Number(value) !== 1
  if (model.capabilities?.audio === true || distinct(model.audio_ratio) || distinct(model.audio_completion_ratio))
    rows.push(['audioIn', t('pricing:audioInPrice')], ['audioOut', t('pricing:audioOutPrice')])
  if (model.capabilities?.vision === true || distinct(model.image_ratio)) rows.push(['imageIn', t('pricing:imageInPrice')])
  const groupName = (g: PricingGroup) => g.name || (g.code === 'default' ? t('flow:defaultGroup') : g.code)
  return <Drawer open onClose={onClose} title={model.display_name || model.model} description={t('catalog:detailHint')} size="lg">
    <div className="mb-6 flex items-center gap-4">
      <VendorIcon vendor={vendor} size="lg" />
      <div className="min-w-0"><p className="mb-1 font-medium">{vendor.name || t('catalog:otherVendor')}</p><ModelId model={model} /></div>
    </div>
    <div className="sticky -top-4 z-10 mb-5 bg-card pb-3 pt-1"><Tabs id="model-detail-tabs" ariaLabel={t('catalog:detailSections')} active={tab} onChange={onTab}
      items={[{ id: 'details', label: t('catalog:details'), icon: ReceiptText, panelId: 'model-details-panel' }, { id: 'code', label: t('catalog:examples'), icon: Terminal, panelId: 'model-code-panel' }]} /></div>
    <div role="tabpanel" id="model-details-panel" aria-labelledby="model-detail-tabs-details" hidden={tab !== 'details'}>
    <FieldGroup title={t('catalog:specifications')}>
      <dl className="grid grid-cols-2 gap-3 rounded-lg bg-muted/40 p-4 text-sm">
        {[[t('catalog:context'), model.context_window], [t('catalog:maxOutput'), model.max_output]].map(([label, value]) =>
          <div key={label}><dt className="text-xs text-muted-foreground">{label}</dt><dd className="mt-1 font-medium">{typeof value === 'number' && value > 0 ? `${value.toLocaleString(i18n.language)} tokens` : t('catalog:notProvided')}</dd></div>)}
      </dl>
      <div className="flex flex-wrap gap-2">{caps.length ? caps.map((cap) => <Badge key={cap} variant="outline">{t(`catalog:cap_${cap}`)}</Badge>) : <p className="text-xs text-muted-foreground">{t('catalog:noCapabilities')}</p>}</div>
    </FieldGroup>
    <FieldGroup title={t('catalog:prices')} hint={t('catalog:priceNote')}>
      <div className="flex flex-wrap items-center justify-between gap-2"><Label htmlFor="detail-group">{t('pricing:viewAsGroup')}</Label>
        <Select id="detail-group" className="max-w-full" value={group} onChange={onGroup} placeholder={t('pricing:baseGroup')}
          options={[...groups.map((g) => ({ value: g.code, label: `${groupName(g)} ×${formatRatio(g.ratio)}` })),
            ...(group && !groups.some((g) => g.code === group) ? [{ value: group, label: group }] : [])]} /></div>
      <ModelPriceSummary model={model} factor={factor} unit={unit} />
      {model.mode === 'ratio' ? <dl className="divide-y divide-border">
        {rows.map(([field, label]) => <div key={field} className="flex items-center justify-between gap-3 py-2.5 text-sm"><dt className="text-muted-foreground">{label}</dt><dd className="text-right font-medium">{formatUnitPrice(modelPrice(model, field, factor, unit), i18n.language)}<span className="ml-2 text-xs font-normal text-muted-foreground">/ {unit} tokens</span></dd></div>)}
      </dl> : model.mode !== 'per_call' && <p className="text-sm leading-6 text-muted-foreground">{t('catalog:tieredHint')}</p>}
    </FieldGroup>
    <FieldGroup title={t('catalog:servingGroups')} hint={t('catalog:availabilityHint')}>
      <div><ModelAvailability model={model} group={group} /></div>
      {model.groups.length > 0 && <div className="flex flex-wrap gap-2">{model.groups.map((code) => {
        const info = groups.find((g) => g.code === code)
        return <button key={code} type="button" onClick={() => onGroup(code)} aria-pressed={group === code}
          className={cn('flex max-w-full items-center gap-2 rounded-lg border px-3 py-2 text-sm outline-none focus-visible:ring-2 focus-visible:ring-primary/40',
            group === code ? 'border-primary/40 bg-primary/5 text-primary' : 'border-border hover:bg-muted')}>
          <span className="truncate">{info ? groupName(info) : code}</span><span className="text-xs text-muted-foreground">×{formatRatio(info?.ratio)}</span>
        </button>
      })}</div>}
    </FieldGroup>
    {(model.mode === 'ratio' || model.mode === 'per_call') && <Simulator key={model.model} model={model} groupFactor={factor} />}
    </div>
    <div role="tabpanel" id="model-code-panel" aria-labelledby="model-detail-tabs-code" hidden={tab !== 'code'}><RequestExamples model={model} /></div>
  </Drawer>
}
