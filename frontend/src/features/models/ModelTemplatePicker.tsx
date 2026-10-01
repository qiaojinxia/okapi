import { useQuery } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { AutocompleteInput } from '@/components/ui/autocomplete-input'
import { Button } from '@/components/ui/button'
import { Label } from '@/components/ui/input'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { qk } from '@/lib/query-keys'
import type { ModelListRow } from './types'

export function ModelTemplatePicker({ onApply }: { onApply: (model: ModelListRow) => void }) {
  const { t } = useTranslation()
  const [draft, setDraft] = useState('')
  const [query, setQuery] = useState('')
  const [selected, setSelected] = useState<ModelListRow | null>(null)
  const templates = useQuery({
    queryKey: [...qk.adminModels, 'templates', query],
    queryFn: () => apiFetch<{ data: ModelListRow[]; total: number }>(`/admin/models?limit=20&q=${encodeURIComponent(query)}`),
    staleTime: 60_000,
  })
  return <div className="space-y-3">
    <Label htmlFor="model-template">{t('admin:modelSimple.templateLabel')}</Label>
    <div className="flex gap-2">
      <AutocompleteInput id="model-template" className="flex-1" search value={draft}
        onChange={(value) => { setDraft(value); setSelected(null) }} onSubmit={() => setQuery(draft.trim())}
        onChoose={(name) => setSelected(templates.data?.data.find((model) => model.model_name === name) ?? null)}
        options={(templates.data?.data ?? []).map((model) => ({ value: model.model_name, label: model.display_name ?? undefined, description: model.vendor ?? undefined }))}
        loading={templates.isPending} error={templates.isError ? describeError(templates.error) : undefined}
        emptyHint={t('admin:modelSimple.templateEmpty')} moreHint={t('admin:modelSimple.templateSearchHint')} />
      <Button variant="outline" onClick={() => { setSelected(null); setQuery(draft.trim()) }}>{t('common:search')}</Button>
    </div>
    {selected && <div className="rounded-lg bg-muted/50 p-3 text-xs leading-5">
      <p className="font-medium">{t('admin:modelSimple.templateSource', { name: selected.model_name })}</p>
      <p className="mt-1 text-muted-foreground">{t('admin:modelSimple.templateConfirm')}</p>
      <Button size="sm" variant="outline" className="mt-3" onClick={() => { onApply(selected); setSelected(null) }}>{t('admin:modelSimple.applyTemplate')}</Button>
    </div>}
  </div>
}
