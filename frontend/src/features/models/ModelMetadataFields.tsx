import { useTranslation } from 'react-i18next'
import type { Dispatch, SetStateAction } from 'react'
import { MODEL_KINDS } from './types'
import { ModelCapabilityTags } from './ModelCapabilityTags'
import { ModelOptionsSection } from './ModelOptionsSection'
import type { MetadataDraft } from './model-config'
import { Checkbox } from '@/components/ui/checkbox'
import { FieldGroup } from '@/components/ui/drawer'
import { Input, Label } from '@/components/ui/input'
import { Select } from '@/components/ui/select'

export function ModelMetadataFields({ value, onChange, capabilities = false, basic = false }: {
  value: MetadataDraft; onChange: Dispatch<SetStateAction<MetadataDraft>>; capabilities?: boolean; basic?: boolean
}) {
  const { t } = useTranslation()
  const field = (key: 'display_name' | 'vendor' | 'description' | 'context_window' | 'max_output') => (
    <div className="flex min-w-0 flex-col gap-1.5" key={key}>
      <Label htmlFor={`meta-${key}`}>{t(`admin:modelMeta.${key}`)}</Label>
      <Input id={`meta-${key}`} value={value[key]} maxLength={key === 'vendor' ? 64 : key === 'description' ? 2000 : 128}
        inputMode={key === 'context_window' || key === 'max_output' ? 'numeric' : undefined}
        placeholder={key === 'context_window' || key === 'max_output' ? t('admin:modelMeta.unknown') : undefined}
        onChange={(e) => { const next = e.target.value; onChange((previous) => ({ ...previous, [key]: next })) }} />
    </div>
  )
  if (basic) return <div className="grid grid-cols-2 gap-3">
    {field('vendor')}
    <div className="flex flex-col gap-1.5"><Label htmlFor="meta-kind">{t('admin:modelMeta.kind')}</Label>
      <Select id="meta-kind" value={value.kind} onChange={(kind) => onChange((previous) => ({ ...previous, kind }))}
        className="w-full" placeholder={t('admin:modelMeta.unknown')}
        options={MODEL_KINDS.map((kind) => ({ value: kind, label: t(`admin:modelKinds.${kind}`) }))} />
    </div>
  </div>
  if (capabilities) return <>
    <ModelOptionsSection id="model-limits-section" title={t('admin:modelMeta.limits')}
      configured={Boolean(value.context_window || value.max_output)}>
      <p className="mb-3 text-xs leading-5 text-muted-foreground">{t('admin:modelMeta.limitsHint')}</p>
      <div className="grid grid-cols-2 gap-3">{field('context_window')}{field('max_output')}</div>
    </ModelOptionsSection>
    <ModelCapabilityTags value={value} onChange={onChange} />
  </>
  return <>
    <FieldGroup title={t('admin:modelMeta.identity')}>
      <div className="grid grid-cols-2 gap-3">{field('display_name')}{field('description')}</div>
    </FieldGroup>
    <FieldGroup title={t('admin:modelMeta.modalities')} hint={t('admin:modelMeta.modalitiesHint')}>
      <div className="grid grid-cols-2 gap-3">{(['input_modalities', 'output_modalities'] as const).map((axis) => <div key={axis} className="rounded-lg border border-border p-3">
        <p className="mb-3 text-sm font-medium">{t(`admin:modelMeta.${axis}`)}</p>
        <div className="grid grid-cols-2 gap-3">{['text', 'image', 'audio', 'video'].map((modality) => <Checkbox key={modality}
          label={t(`admin:modelModalities.${modality}`)} srLabel={`${t(`admin:modelMeta.${axis}`)} · ${t(`admin:modelModalities.${modality}`)}`}
          checked={value[axis].includes(modality)} onChange={(checked) => onChange((previous) => ({ ...previous,
            [axis]: checked ? [...previous[axis], modality] : previous[axis].filter((m) => m !== modality) }))} />)}</div>
      </div>)}</div>
    </FieldGroup>
  </>
}
