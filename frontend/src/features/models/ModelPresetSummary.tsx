import { ExternalLink } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { referencePriceAvailable, type ModelPreset } from './model-presets'
import type { MetadataDraft } from './model-config'

export function ModelPresetSummary({ preset, metadata, applied, canApplyPrice, onApplyPrice }: {
  preset: ModelPreset; metadata: MetadataDraft; applied: boolean; canApplyPrice: boolean; onApplyPrice: () => void
}) {
  const { t } = useTranslation()
  const [showConditions, setShowConditions] = useState(false)
  const vendor = applied ? metadata.vendor : preset.vendor
  const kind = applied ? metadata.kind : preset.kind
  const capabilities = applied ? metadata.capabilities : preset.capabilities
  const badges = [vendor, kind ? t(`admin:modelKinds.${kind}`) : '',
    ...(['vision', 'tools', 'reasoning', 'audio', 'video'] as const).filter((key) => capabilities[key] === true).map((key) => t(`admin:modelCaps.${key}`))].filter(Boolean)
  return <div className="space-y-2 rounded-lg border border-primary/15 bg-primary/5 px-3 py-2.5" data-testid="model-preset-summary">
    <div className="flex flex-wrap items-center justify-between gap-2 text-xs">
      <span className="font-medium text-primary">{t(applied ? 'admin:modelPreset.applied' : 'admin:modelPreset.referenceOnly')}</span>
      <a className="inline-flex items-center gap-1 text-muted-foreground hover:text-primary" href={preset.source} target="_blank" rel="noopener noreferrer">
        {t('admin:modelPreset.source', { date: preset.checkedAt })}<ExternalLink aria-hidden className="h-3 w-3" />
      </a>
    </div>
    <div className="flex flex-wrap gap-1.5">{badges.map((label) => <span key={label} className="rounded bg-background/80 px-1.5 py-0.5 text-xs text-muted-foreground">{label}</span>)}</div>
    {preset.cacheTtls?.length ? <p className="text-xs text-muted-foreground">{t('admin:modelPreset.ttls', { values: preset.cacheTtls.join(' / ') })}</p> : null}
    {preset.notices?.includes('partial') && <p className="text-xs leading-5 text-amber-600 dark:text-amber-400">{t('admin:modelPresetNotices.partial')}</p>}
    {preset.notices?.includes('imagePricing') && <p className="text-xs leading-5 text-amber-600 dark:text-amber-400">{t('admin:modelPresetNotices.imagePricing')}</p>}
    {preset.referencePrice && <div className="flex flex-wrap items-center justify-between gap-2 border-t border-primary/10 pt-2">
      <span className="text-xs text-muted-foreground">{t('admin:modelPreset.priceReference', preset.referencePrice)}
        {preset.pricingSource && <a href={preset.pricingSource} target="_blank" rel="noopener noreferrer" className="ml-1 text-primary hover:underline">{t('admin:modelPreset.pricingSource')}</a>}</span>
      <Button size="sm" variant="outline" disabled={!canApplyPrice || !referencePriceAvailable(preset)} onClick={() => { setShowConditions(true); onApplyPrice() }}>
        {t(referencePriceAvailable(preset) ? 'admin:modelPreset.applyPrice' : 'admin:modelPreset.priceExpired')}
      </Button>
    </div>}
    <details open={showConditions} onToggle={(event) => setShowConditions(event.currentTarget.open)} className="text-xs leading-5 text-muted-foreground">
      <summary className="cursor-pointer text-primary">{t('admin:modelPreset.conditions')}</summary>
      <p className="mt-1">{t('admin:modelPreset.scope')}</p>
      {applied && <p>{t('admin:modelPreset.customResetHint')}</p>}
      {preset.cache && <p>{t('admin:modelPreset.cachePrecision')}</p>}
      {preset.notices?.map((notice) => <p key={notice}>{t(`admin:modelPresetNotices.${notice}`)}</p>)}
    </details>
  </div>
}
