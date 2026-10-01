import { Plus, Trash2 } from 'lucide-react'
import { useMutation } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { ModelListRow } from '@/features/models/types'
import { AXIS_LABEL, INDEPENDENT_AXES, MODAL_AXES } from '@/features/models/types'
import { metadataDraft, metadataPayload, priceFromMicro, pricingAxes, pricesFromRatios, ratiosFromPrices, usdMicro, validLimit, validRatio } from './model-config'
import { ModelMetadataFields } from './ModelMetadataFields'
import { ModelOptionsSection } from './ModelOptionsSection'
import { ModelTemplatePicker } from './ModelTemplatePicker'
import { ModelPresetSummary } from './ModelPresetSummary'
import { ModelRateField } from './ModelRateField'
import { pricingDisabledReason, type PricingAxis } from './model-pricing-availability'
import { MODEL_PRESETS, findModelPreset, presetCache, presetMetadata, referencePriceAvailable, type ModelPreset } from './model-presets'
import { AutocompleteInput } from '@/components/ui/autocomplete-input'
import { Button } from '@/components/ui/button'
import { Drawer, FieldGroup } from '@/components/ui/drawer'
import { IconButton } from '@/components/ui/icon-button'
import { Input, Label } from '@/components/ui/input'
import { Select } from '@/components/ui/select'
import { ModelTagsInput } from '@/features/models/model-input'
import { toast } from '@/components/ui/toast'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'

/// Presets only change this draft. Catalog declarations never enable a channel.
export function ModelDrawer({
  model,
  basePriceMicro = 2_000_000,
  onClose,
  onDone,
}: {
  model: ModelListRow | undefined
  basePriceMicro?: number
  onClose: () => void
  onDone: () => void
}) {
  const { t } = useTranslation()
  const [name, setName] = useState(model?.model_name ?? '')
  const [metadata, setMetadata] = useState(() => metadataDraft(model))
  const [mode, setMode] = useState(model?.pricing_mode ?? 'ratio')
  const [callPrice, setCallPrice] = useState(priceFromMicro(model?.per_call_price_micro))
  const [independent, setIndependent] = useState<Record<string, string>>(model?.modality_ratios ?? {})
  const [axes, setAxes] = useState(() => pricingAxes(model))
  const [prices, setPrices] = useState(() => pricesFromRatios(basePriceMicro, axes.model_ratio, axes.completion_ratio))
  const [templateSource, setTemplateSource] = useState('')
  const [appliedPreset, setAppliedPreset] = useState<ModelPreset | null>(null)
  const [presetVendor, setPresetVendor] = useState('')
  const presetVendors = [...new Set(MODEL_PRESETS.map((preset) => preset.vendor))]
  const matchingPresets = MODEL_PRESETS.filter((preset) => presetVendor === '' || preset.vendor === presetVendor)
  const referencePreset = appliedPreset ?? (model ? findModelPreset(model.model_name) : undefined)
  const [tiers, setTiers] = useState<{ tier: string; ratio: string }[]>(() => Object.entries(model?.tier_ratios ?? {}).map(([tier, ratio]) => ({ tier, ratio: String(ratio) })))
  // 阶梯计价表 "0:2.5,128000:5"（from_tokens:USD_per_1M）。空串 = ratio 模式。
  // 与 model_ratio 互斥：填了阶梯，每 token 基准单价改由查表得出，model_ratio 不再参与。
  const [tierExpr, setTierExpr] = useState(model?.tier_expr ?? '')
  // 从现值起手：编辑倍率时不该顺手清掉已配的降级链（后端 None=不改动，
  // 但始终回传现值让"清空"也是显式操作）
  const [fallbacks, setFallbacks] = useState<string[]>(model?.fallback_models ?? [])
  const namedTiers = tiers.filter((x) => x.tier.trim() !== '')
  const conversion = ratiosFromPrices(basePriceMicro, prices.input, prices.output)
  const valid = name.trim() !== '' && name.trim().length <= 128
    && validLimit(metadata.context_window) && validLimit(metadata.max_output)
    && Object.values(axes).every(validRatio)
    && Object.values(independent).every((v) => v.trim() === '' || validRatio(v))
    && namedTiers.every((x) => validRatio(x.ratio) && x.tier.trim().length <= 64)
    && new Set(namedTiers.map((x) => x.tier.trim())).size === namedTiers.length
    && (mode !== 'per_call' || usdMicro(callPrice) !== null)
    && (mode !== 'tiered' || tierExpr.trim() !== '')
    && (mode !== 'ratio' || conversion !== null)

  const upsert = useMutation({
    mutationFn: () =>
      apiFetch('/admin/models', {
        method: 'POST',
        body: {
          model_name: name.trim(),
          ...Object.fromEntries(Object.entries(axes).map(([key, value]) => [key, value.trim()])),
          metadata: metadataPayload(metadata),
          pricing_mode: mode,
          per_call_price_micro: mode === 'per_call' ? usdMicro(callPrice) : undefined,
          modality_ratios: Object.fromEntries(Object.entries(independent).filter(([, v]) => v.trim() !== '').map(([k, v]) => [k, v.trim()])),
          // {} explicitly clears tiers; omission would preserve a deleted old tier.
          tier_ratios: Object.fromEntries(namedTiers.map((x) => [x.tier.trim(), x.ratio.trim()])),
          fallback_models: fallbacks,
          // 始终回传：空串是"切回 ratio"的显式表达，undefined 才是"不改动"
          tier_expr: mode === 'tiered' ? tierExpr.trim() : '',
        },
      }),
    onSuccess: () => {
      toast.success(t('admin:modelMeta.saved'))
      onDone()
      onClose()
    },
    onError: (err) => toast.error(describeError(err)),
  })

  const changePrices = (key: 'input' | 'output', value: string) => {
    const next = { ...prices, [key]: value }
    setPrices(next)
    const converted = ratiosFromPrices(basePriceMicro, next.input, next.output)
    if (converted) setAxes((previous) => ({ ...previous, model_ratio: converted.model_ratio, completion_ratio: converted.completion_ratio }))
  }
  const changeAxis = (key: string, value: string) => {
    const next = { ...axes, [key]: value }
    setAxes(next)
    if (key === 'model_ratio' || key === 'completion_ratio') setPrices(pricesFromRatios(basePriceMicro, next.model_ratio, next.completion_ratio))
  }
  const applyTemplate = (source: ModelListRow) => {
    const next = pricingAxes(source)
    setMetadata(metadataDraft(source)); setAxes(next)
    setPrices(pricesFromRatios(basePriceMicro, next.model_ratio, next.completion_ratio))
    setMode(source.pricing_mode ?? 'ratio')
    setCallPrice(priceFromMicro(source.per_call_price_micro))
    setIndependent(source.modality_ratios ?? {})
    setTiers(Object.entries(source.tier_ratios ?? {}).map(([tier, ratio]) => ({ tier, ratio: String(ratio) })))
    setTierExpr(source.tier_expr ?? ''); setTemplateSource(source.model_name)
    setAppliedPreset(null)
    // Identity and fallback routing are not copied by a pricing template.
  }
  const changePresetCache = (preset?: ModelPreset) => {
    const cache = preset ? presetCache(preset) : { cache_ratio: '1', cache_write_ratio: '1', ttl: {} }
    setAxes((previous) => ({ ...previous, cache_ratio: cache.cache_ratio, cache_write_ratio: cache.cache_write_ratio }))
    setIndependent((previous) => ({ ...Object.fromEntries(Object.entries(previous)
      .filter(([key]) => key !== 'cache_write_5m' && key !== 'cache_write_1h')), ...cache.ttl }))
  }
  const applyPreset = (preset: ModelPreset) => {
    setName(preset.id); setMetadata(presetMetadata(preset)); setAppliedPreset(preset); setTemplateSource('')
    changePresetCache(preset)
    // Selecting a specification never replaces entered input/output prices or routing.
  }
  const changeName = (value: string) => {
    setName(value)
    if (appliedPreset && value !== appliedPreset.id) {
      setAppliedPreset(null); setMetadata(metadataDraft()); changePresetCache()
    }
  }
  const applyReferencePrice = () => {
    if (!referencePreset || !referencePriceAvailable(referencePreset) || mode !== 'ratio') return
    const price = referencePreset.referencePrice!
    const converted = ratiosFromPrices(basePriceMicro, price.input, price.output)
    setPrices({ input: price.input, output: price.output })
    if (converted) setAxes((previous) => ({ ...previous, model_ratio: converted.model_ratio, completion_ratio: converted.completion_ratio }))
  }
  const advancedConfigured = Object.keys(independent).length > 0 || namedTiers.length > 0 || fallbacks.length > 0
    || ['cache_ratio', 'cache_write_ratio', ...MODAL_AXES].some((key) => Number(axes[key]) !== 1)
  const metadataConfigured = Boolean(metadata.display_name || metadata.description || metadata.context_window || metadata.max_output
    || metadata.input_modalities.length || metadata.output_modalities.length || Object.keys(metadata.capabilities).length)

  const axisField = (key: keyof typeof AXIS_LABEL) => (
    <ModelRateField key={key} id={`ax-${key}`} label={t(AXIS_LABEL[key])} value={axes[key] ?? '1'}
      disabledReason={pricingDisabledReason(metadata, key)} onChange={(value) => changeAxis(key, value)} />
  )
  const independentField = (key: typeof INDEPENDENT_AXES[number]) => (
    <ModelRateField key={key} id={`ind-${key}`} label={t(`admin:modelIndependent.${key}`)} value={independent[key] ?? ''}
      placeholder={t('admin:modelMeta.inherit')} disabledReason={pricingDisabledReason(metadata, key)}
      onChange={(value) => setIndependent((previous) => ({ ...previous, [key]: value }))} />
  )
  const rateField = (key: PricingAxis) => key in AXIS_LABEL
    ? axisField(key as keyof typeof AXIS_LABEL) : independentField(key as typeof INDEPENDENT_AXES[number])
  const applicable = <T extends PricingAxis>(keys: readonly T[]) => keys.filter((key) => !pricingDisabledReason(metadata, key))
  const cacheAxes = applicable(['cache_ratio', 'cache_write_ratio'] as const)
  const ttlAxes = applicable(INDEPENDENT_AXES.slice(0, 2))
  const modalAxes = applicable([...MODAL_AXES, 'image_output'] as const)
  const modalCacheAxes = applicable(INDEPENDENT_AXES.slice(2, -1))
  const unavailableAxes = (['cache_ratio', 'cache_write_ratio', ...MODAL_AXES, ...INDEPENDENT_AXES] as const)
    .filter((key) => pricingDisabledReason(metadata, key))
  const hasUnavailableValues = unavailableAxes.some((key) => key in AXIS_LABEL ? Number(axes[key]) !== 1 : Boolean(independent[key]?.trim()))

  return (
    <Drawer
      open
      size="lg"
      onClose={onClose}
      title={model ? t('admin:editModel', { name: model.model_name }) : t('admin:createModel')}
      description={t('admin:modelSimple.drawerHint')}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>
            {t('common:cancel')}
          </Button>
          <Button disabled={!valid || upsert.isPending} onClick={() => upsert.mutate()}>
            {t('common:save')}
          </Button>
        </>
      }
    >
      <div className="space-y-3">
      <section className="space-y-3" aria-label={t('common:basicInfo')}>
        <div className={model ? '' : 'grid items-end gap-3 sm:grid-cols-[11rem_minmax(0,1fr)]'}>
        {!model && <div className="flex min-w-0 flex-col gap-1.5">
          <Label htmlFor="m-preset-vendor">{t('admin:modelPreset.vendorFilter')}</Label>
          <Select id="m-preset-vendor" className="w-full" value={presetVendor} onChange={setPresetVendor}
            placeholder={t('admin:modelPreset.allVendors')}
            options={presetVendors.map((vendor) => ({ value: vendor,
              label: `${t(`admin:modelPreset.vendors.${vendor}`, { defaultValue: vendor })} (${MODEL_PRESETS.filter((preset) => preset.vendor === vendor).length})` }))} />
        </div>}
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="m-name">{t('admin:modelName')}</Label>
          <AutocompleteInput
            key={presetVendor}
            id="m-name"
            inputClassName="font-mono text-sm"
            value={name}
            readOnly={model !== undefined}
            browseOnFocus
            placeholder={t('admin:modelPreset.placeholder')}
            onChange={changeName}
            onChoose={(id) => { const preset = matchingPresets.find((preset) => preset.id === id); if (preset) applyPreset(preset) }}
            options={matchingPresets.map((preset) => ({ value: preset.id, label: preset.displayName, description: preset.vendor }))}
            emptyHint={t('admin:modelPreset.customHint')}
          />
        </div>
        </div>
        {referencePreset && <ModelPresetSummary preset={referencePreset} metadata={metadata} applied={Boolean(appliedPreset)}
          canApplyPrice={mode === 'ratio'} onApplyPrice={applyReferencePrice} />}
        {!model && !appliedPreset && <p className="text-xs leading-5 text-muted-foreground">{t('admin:modelPreset.selectHint', { n: matchingPresets.length })}</p>}
      </section>
      <FieldGroup title={t('admin:modelSimple.basicPricing')} hint={t('admin:modelSimple.priceHint')}>
        <div className="sm:w-1/2 sm:pr-1.5">
        <div className="flex flex-col gap-1.5"><Label htmlFor="m-pricing-mode">{t('admin:modelMeta.billingMode')}</Label>
        <Select id="m-pricing-mode" className="w-full" value={mode}
          onChange={(value) => {
            setMode(value)
            if (value === 'ratio') { setTierExpr(''); setPrices(pricesFromRatios(basePriceMicro, axes.model_ratio, axes.completion_ratio)) }
          }} options={['ratio', 'per_call', 'tiered'].map((value) => ({ value, label: t(`admin:modelSimple.mode_${value}`) }))} /></div>
        </div>
        {mode === 'per_call' && <div className="flex flex-col gap-1.5"><Label htmlFor="m-call-price">{t('admin:modelMeta.callPrice')}</Label>
          <Input id="m-call-price" value={callPrice} inputMode="decimal" onChange={(e) => setCallPrice(e.target.value)} placeholder="0.01" />
        </div>}
        {mode === 'ratio' && <>
          <div className="grid grid-cols-2 gap-3">{(['input', 'output'] as const).map((key) => <div className="flex min-w-0 flex-col gap-1.5" key={key}>
            <Label htmlFor={`price-${key}`}>{t(`admin:modelSimple.price_${key}`)}</Label>
            <Input id={`price-${key}`} value={prices[key]} inputMode="decimal" onChange={(e) => changePrices(key, e.target.value)} />
          </div>)}</div>
          {!conversion && <p role="alert" className="text-xs leading-5 text-destructive">{t('admin:modelSimple.priceInvalid')}</p>}
          {conversion?.approximate && <p role="status" className="rounded-lg bg-muted/60 p-3 text-xs leading-5 text-muted-foreground">
            {t('admin:modelSimple.priceRounded', { input: conversion.effective.input, output: conversion.effective.output })}</p>}
          <p className="text-xs leading-5 text-muted-foreground">{t('admin:modelSimple.baseHint', { price: basePriceMicro / 1_000_000 })}</p>
        </>}
        {mode === 'tiered' && <>
          <div className="flex flex-col gap-1.5"><Label htmlFor="m-tier-expr">{t('admin:tierExpr')}</Label>
            <Input id="m-tier-expr" className="font-mono" placeholder="0:2.5,128000:5" value={tierExpr} onChange={(e) => setTierExpr(e.target.value)} /></div>
          <p className="text-xs leading-5 text-muted-foreground">{t('admin:tierExprHint')}</p>
          <div className="grid grid-cols-2 gap-3">{axisField('completion_ratio')}</div>
        </>}
      </FieldGroup>

      <ModelOptionsSection id="model-metadata-section" title={t('admin:modelSimple.metadataTitle')}
        hint={t(metadataConfigured ? 'admin:modelSimple.metadataConfigured' : 'admin:modelSimple.metadataHint')} configured={metadataConfigured}>
        {model && referencePreset && <div className="space-y-2 rounded-lg bg-muted/50 p-3">
          <p className="text-xs leading-5 text-muted-foreground">{t('admin:modelPreset.replaceHint')}</p>
          <Button size="sm" variant="outline" onClick={() => {
            setMetadata((previous) => ({ ...presetMetadata(referencePreset), display_name: previous.display_name || referencePreset.displayName,
              description: previous.description }))
            setAppliedPreset(referencePreset)
          }}>{t('admin:modelPreset.replaceSpecs')}</Button>
        </div>}
        <ModelMetadataFields value={metadata} onChange={setMetadata} basic />
        <ModelMetadataFields value={metadata} onChange={setMetadata} />
        <ModelMetadataFields value={metadata} onChange={setMetadata} capabilities />
        {!model && <ModelOptionsSection id="model-template-section" title={t('admin:modelSimple.templateTitle')}
          hint={templateSource ? t('admin:modelSimple.templateSource', { name: templateSource }) : undefined} configured={Boolean(templateSource)}>
          <p className="mb-3 text-xs leading-5 text-muted-foreground">{t('admin:modelSimple.templateHint')}</p>
          <ModelTemplatePicker onApply={applyTemplate} />
        </ModelOptionsSection>}
      </ModelOptionsSection>
      <ModelOptionsSection id="model-advanced-section" title={t('admin:modelSimple.advancedTitle')}
        hint={t(advancedConfigured ? 'admin:modelSimple.advancedConfigured' : 'admin:modelSimple.advancedHint')} configured={advancedConfigured}>
      <div className="space-y-3">
      {mode === 'ratio' && <ModelOptionsSection id="model-ratio-editor" title={t('admin:modelSimple.ratioEditor')}>
        <div className="grid grid-cols-2 gap-3">{axisField('model_ratio')}{axisField('completion_ratio')}</div>
      </ModelOptionsSection>}
      {mode !== 'per_call' && <>
      {(cacheAxes.length > 0 || ttlAxes.length > 0) && <ModelOptionsSection id="model-cache-section" title={t('admin:modelSimple.cacheTitle')}
        configured={Number(axes.cache_ratio) !== 1 || Number(axes.cache_write_ratio) !== 1 || Boolean(independent.cache_write_5m || independent.cache_write_1h)}>
      <div className="space-y-3">
        {cacheAxes.length > 0 && <div className="grid grid-cols-2 gap-3">{cacheAxes.map(axisField)}</div>}
        {ttlAxes.length > 0 && <ModelOptionsSection id="model-cache-ttl-section" title={t('admin:modelSimple.ttlRates')}
          configured={ttlAxes.some((key) => Boolean(independent[key]?.trim()))}>
          <p className="mb-3 text-xs leading-5 text-muted-foreground">{t('admin:modelMeta.independentHint')}</p>
          <div className="grid grid-cols-2 gap-3">{ttlAxes.map(independentField)}</div>
        </ModelOptionsSection>}
      </div>
      </ModelOptionsSection>}
      {(modalAxes.length > 0 || modalCacheAxes.length > 0) && <ModelOptionsSection id="model-modal-section" title={t('admin:modelSimple.modalTitle')}
        configured={MODAL_AXES.some((key) => Number(axes[key]) !== 1) || INDEPENDENT_AXES.slice(2).some((key) => independent[key] !== undefined)}>
      <div className="space-y-3">
        {modalAxes.length > 0 && <div className="grid grid-cols-2 gap-3">{modalAxes.map(rateField)}</div>}
        {modalCacheAxes.length > 0 && <ModelOptionsSection id="model-modal-cache-section" title={t('admin:modelSimple.modalCacheTitle')}
          configured={modalCacheAxes.some((key) => Boolean(independent[key]?.trim()))}>
          <p className="mb-3 text-xs leading-5 text-muted-foreground">{t('admin:modelMeta.independentHint')}</p>
          <div className="grid grid-cols-2 gap-3">{modalCacheAxes.map(independentField)}</div>
        </ModelOptionsSection>}
      </div>
      </ModelOptionsSection>}
      {unavailableAxes.length > 0 && <ModelOptionsSection id="model-unavailable-section"
        title={t('admin:modelSimple.unavailableTitle', { n: unavailableAxes.length })} configured={hasUnavailableValues}>
        <p className="mb-3 text-xs leading-5 text-muted-foreground">{t('admin:modelMeta.rateAvailabilityHint')}</p>
        <div className="grid grid-cols-2 gap-3">{unavailableAxes.map(rateField)}</div>
      </ModelOptionsSection>}
      <ModelOptionsSection id="model-billing-notes" title={t('admin:modelSimple.billingNotes')}>
        <div className="space-y-2 text-xs leading-5 text-muted-foreground">
          <p>{t('admin:cachePricingHint', { read: axes.cache_ratio, write: axes.cache_write_ratio })}</p>
          <p>{t('admin:modelMeta.independentHint')}</p>
          <p>{t('admin:modelMeta.ttlHint')}</p>
          <p>{t('admin:axesModalHint')}</p>
        </div>
      </ModelOptionsSection>
      </>}
      <ModelOptionsSection id="model-fallback-section" title={t('admin:fallbackModels')} hint={t('admin:fallbackModelsHint')} configured={fallbacks.length > 0}>
      <FieldGroup title={t('admin:fallbackModels')} hint={t('admin:fallbackModelsHint')}>
        <ModelTagsInput
          id="m-fallbacks"
          value={fallbacks}
          onChange={setFallbacks}
          placeholder="gpt-4o-mini"
        />
      </FieldGroup>
      </ModelOptionsSection>
      <ModelOptionsSection id="model-tiers-section" title={t('admin:tierRatios')} hint={t('admin:tierRatiosHint')} configured={namedTiers.length > 0}>
      <FieldGroup title={t('admin:tierRatios')} hint={t('admin:tierRatiosHint')}>
        {tiers.map((row, i) => (
          <div key={i} className="flex items-end gap-2">
            <div className="flex flex-1 flex-col gap-1.5">
              <Label htmlFor={`tier-${i}`}>{t('admin:tierName')}</Label>
              <Input
                id={`tier-${i}`}
                value={row.tier}
                placeholder="flex"
                onChange={(e) => { const value = e.target.value; setTiers((ts) => ts.map((x, j) => (i === j ? { ...x, tier: value } : x))) }}
              />
            </div>
            <div className="flex w-28 flex-col gap-1.5">
              <Label htmlFor={`tratio-${i}`}>{t('admin:ruleMultiplier')}</Label>
              <Input
                id={`tratio-${i}`}
                value={row.ratio}
                inputMode="decimal"
                onChange={(e) => { const value = e.target.value; setTiers((ts) => ts.map((x, j) => (i === j ? { ...x, ratio: value } : x))) }}
              />
            </div>
            <IconButton
              icon={Trash2}
              label={t('common:delete')}
              variant="destructive"
              onClick={() => setTiers((ts) => ts.filter((_, j) => j !== i))}
            />
          </div>
        ))}
        <Button
          size="sm"
          variant="outline"
          className="self-start"
          onClick={() => setTiers((ts) => [...ts, { tier: '', ratio: '1' }])}
        >
          <Plus className="h-4 w-4" />
          {t('admin:tierAdd')}
        </Button>
      </FieldGroup>
      </ModelOptionsSection>
      </div>
      </ModelOptionsSection>
      {!valid && <p role="status" className="rounded-lg bg-muted px-3 py-2 text-xs text-muted-foreground">{t('admin:modelMeta.validationHint')}</p>}
      </div>
    </Drawer>
  )
}
