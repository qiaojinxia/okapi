import { ChevronDown } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import type { Dispatch, SetStateAction } from 'react'
import type { MetadataDraft } from './model-config'
import { Badge } from '@/components/ui/badge'
import { FieldGroup } from '@/components/ui/drawer'
import { Label } from '@/components/ui/input'
import { Select } from '@/components/ui/select'

// The manual editor exposes useful catalog labels, not channel or billing switches.
// Other preset/legacy declarations stay in the draft and round-trip unchanged.
export const EDITABLE_CAPABILITIES = ['vision', 'tools', 'reasoning', 'json', 'structured_output'] as const

export function ModelCapabilityTags({ value, onChange }: {
  value: MetadataDraft; onChange: Dispatch<SetStateAction<MetadataDraft>>
}) {
  const { t } = useTranslation()
  const declared = EDITABLE_CAPABILITIES.filter((key) => typeof value.capabilities[key] === 'boolean')
  const retained = Object.keys(value.capabilities).some((key) => !EDITABLE_CAPABILITIES.some((editable) => editable === key))
  return <FieldGroup title={t('admin:modelMeta.capabilities')} hint={t('admin:modelMeta.capabilitiesHint')}>
    <div data-testid="model-capability-summary" className="flex flex-wrap items-center gap-2">
      {declared.map((key) => <Badge key={key} variant={value.capabilities[key] ? 'info' : 'muted'}>
        {t(`admin:modelCaps.${key}`)}{!value.capabilities[key] && ` · ${t('admin:modelMeta.unsupported')}`}
      </Badge>)}
      {declared.length === 0 && <p className="text-xs leading-5 text-muted-foreground">{t('admin:modelMeta.capabilitiesEmpty')}</p>}
    </div>
    <details id="model-capability-editor" className="group/capabilities rounded-lg border border-border">
      <summary className="flex cursor-pointer list-none items-center justify-between gap-3 rounded-lg px-3 py-2 text-xs font-medium outline-none hover:bg-muted/40 focus-visible:ring-2 focus-visible:ring-primary/40 [&::-webkit-details-marker]:hidden">
        {t('admin:modelMeta.editCapabilities')}
        <ChevronDown aria-hidden className="h-3.5 w-3.5 shrink-0 text-muted-foreground transition-transform group-open/capabilities:rotate-180" />
      </summary>
      <div className="divide-y divide-border border-t border-border px-3">
        {EDITABLE_CAPABILITIES.map((key) => <div key={key} className="flex min-w-0 items-center justify-between gap-3 py-2">
          <Label htmlFor={`cap-${key}`} className="min-w-0 text-xs">{t(`admin:modelCaps.${key}`)}</Label>
          <Select id={`cap-${key}`} className="w-28 shrink-0" value={value.capabilities[key] === undefined ? '' : String(value.capabilities[key])}
            placeholder={t('admin:modelMeta.unknown')}
            options={[{ value: 'true', label: t('admin:modelMeta.supported') }, { value: 'false', label: t('admin:modelMeta.unsupported') }]}
            onChange={(next) => onChange((previous) => {
              const caps = { ...previous.capabilities }
              if (next === '') delete caps[key]; else caps[key] = next === 'true'
              return { ...previous, capabilities: caps }
            })} />
        </div>)}
      </div>
    </details>
    {retained && <p className="text-xs leading-5 text-muted-foreground">{t('admin:modelMeta.retainedCapabilities')}</p>}
  </FieldGroup>
}
