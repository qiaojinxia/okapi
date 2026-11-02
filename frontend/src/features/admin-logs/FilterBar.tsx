import { useEffect, useId, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { ChevronDown, SlidersHorizontal, Search } from 'lucide-react'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { CardContent } from '@/components/ui/card'
import { Field } from '@/components/ui/field'
import { Input } from '@/components/ui/input'
import { Select } from '@/components/ui/select'
import { ModelSearchInput } from '@/features/models/model-input'
import { EntitySearchInput, malformedEntityId } from '@/features/entity-search/EntitySearchInput'
import type { EntityKind, EntityOption } from '@/features/entity-search/EntitySearchInput'
import { Switch } from '@/components/ui/switch'
import { invalidId, numericFilters, validRange } from './search'
import type { Draft } from './search'

export function FilterBar({
  draft,
  onChange,
  onApply,
  known,
}: {
  draft: Draft
  onChange: (d: Draft) => void
  onApply: () => void
  known: Partial<Record<EntityKind, EntityOption[]>>
}) {
  const { t } = useTranslation()
  const advancedValues = [draft.user_id, draft.api_key_id, draft.channel_id, draft.error_code, draft.request_id, draft.upstream_request_id, draft.group, draft.client_type, draft.log_type]
  const advancedKey = JSON.stringify(advancedValues)
  const advancedCount = advancedValues.filter((value) => value.trim()).length
  const [advancedOpen, setAdvancedOpen] = useState(advancedCount > 0)
  const advancedId = useId()
  const [blurred, setBlurred] = useState<Partial<Record<(typeof numericFilters)[number], boolean>>>({})
  useEffect(() => { if ((JSON.parse(advancedKey) as string[]).some((value) => value.trim())) setAdvancedOpen(true) }, [advancedKey])
  const invalid = numericFilters.some((field) => invalidId(draft[field])) || Boolean((draft.from || draft.to) && !validRange(draft))
  const text = (
    field: 'error_code' | 'request_id' | 'upstream_request_id' | 'group' | 'client_type',
    label: string,
    placeholder?: string,
  ) => (
    <Field label={label} htmlFor={`lf-${field}`}>
      <Input
        id={`lf-${field}`}
        className="h-11 font-mono text-sm md:h-9 md:text-xs"
        value={draft[field]}
        placeholder={placeholder}
        onChange={(e) => onChange({ ...draft, [field]: e.target.value })}
      />
    </Field>
  )
  const entity = (field: (typeof numericFilters)[number], kind: EntityKind, label: string) => {
    const error = invalidId(draft[field]) && (blurred[field] || malformedEntityId(draft[field]))
    return <Field label={label} htmlFor={`lf-${field}`}>
      <EntitySearchInput id={`lf-${field}`} kind={kind} knownOptions={known[kind]} value={draft[field]}
        inputClassName="h-11 md:h-9"
        aria-invalid={error || undefined} aria-describedby={error ? `lf-${field}-error` : undefined}
        onChange={(value) => { setBlurred((prev) => ({ ...prev, [field]: false })); onChange({ ...draft, [field]: value }) }}
        onBlur={() => setBlurred((prev) => ({ ...prev, [field]: true }))}
        onSubmit={() => { setBlurred((prev) => ({ ...prev, [field]: true })); if (!invalid) onApply() }} />
      {error && <span id={`lf-${field}-error`} className="text-xs text-destructive">{t(malformedEntityId(draft[field]) ? 'analytics:invalidFilterId' : 'analytics:entitySelectionRequired')}</span>}
    </Field>
  }
  const active = [draft.model, draft.user_id, draft.api_key_id, draft.channel_id, draft.error_code, draft.request_id, draft.upstream_request_id, draft.group, draft.client_type, draft.log_type]
    .filter((v) => v.trim() !== '').length + (draft.errors_only ? 1 : 0)
  return (
      <form
        className="min-w-0"
        onSubmit={(e) => {
          e.preventDefault()
          if (!invalid) onApply()
        }}
        onKeyDown={(event) => { if (event.key === 'Enter' && event.nativeEvent.isComposing) event.preventDefault() }}
      >
        <CardContent className="flex flex-col gap-2 p-0 pb-2">
          <div className="flex min-w-0 flex-wrap items-center gap-2 md:gap-3">
            <ModelSearchInput id="lf-model" className="w-full sm:w-64" aria-label={t('pricing:model')}
              inputClassName="h-11 md:h-9" placeholder={t('portal:logsModelHint')}
              value={draft.model} onChange={(model) => onChange({ ...draft, model })} onSubmit={() => { if (!invalid) onApply() }} />
            <Switch
              checked={draft.errors_only}
              onChange={(v) => onChange({ ...draft, errors_only: v })}
              label={t('logs:errorsOnly')}
            />
            <Button type="button" size="sm" variant="ghost" aria-expanded={advancedOpen} aria-controls={advancedId} onClick={() => setAdvancedOpen(!advancedOpen)} className="min-h-11 text-muted-foreground md:min-h-9">
              <SlidersHorizontal aria-hidden className="h-3.5 w-3.5" />{t('admin:logsMoreFilters')}
              {advancedCount > 0 && <Badge variant="muted">{advancedCount}</Badge>}
              <ChevronDown aria-hidden className={`h-3.5 w-3.5 transition-transform ${advancedOpen ? 'rotate-180' : ''}`} />
            </Button>
            <div className="ml-auto flex max-w-full flex-wrap items-center gap-2">
              {active > 0 && (
                <Button
                  type="button"
                  size="sm"
                  variant="ghost"
                  onClick={() =>
                    onChange({
                      ...draft,
                      model: '',
                      user_id: '',
                      api_key_id: '',
                      channel_id: '',
                      error_code: '',
                      request_id: '',
                      upstream_request_id: '', group: '', client_type: '', log_type: '',
                      errors_only: false,
                    })
                  }
                >
                  {t('common:clearFilters')}
                </Button>
              )}
              <Button type="submit" size="sm" className="min-h-11 md:min-h-9" disabled={invalid}>
                <Search className="h-3.5 w-3.5" />
                {t('common:search')}
              </Button>
            </div>
          </div>
          <div id={advancedId} hidden={!advancedOpen} className="min-w-0 border-t border-border/60 pt-2">
            <div className="grid items-start gap-2 md:grid-cols-[repeat(3,minmax(0,20rem))]">
              {entity('user_id', 'user', t('analytics:dimUser'))}
              {entity('api_key_id', 'api_key', t('analytics:dimApiKey'))}
              {entity('channel_id', 'channel', t('analytics:dimChannel'))}
            </div>
            <div className="mt-2 grid gap-2 sm:grid-cols-[minmax(0,20rem)_minmax(0,32rem)]">
              {text('error_code', t('logs:errorCode'), 'upstream_error')}
              {text('request_id', t('logs:requestId'), 'uuid')}
              {text('upstream_request_id', t('logs:upstreamId'))}
              {text('group', t('logs:group'))}
              {text('client_type', t('admin:logsClientType'), 'codex / claude_code / curl')}
              <Field label={t('logs:filterType')} htmlFor="lf-log_type">
                <Select id="lf-log_type" className="w-full" value={draft.log_type} onChange={(log_type) => onChange({ ...draft, log_type })}
                  placeholder={t('common:all')} options={[1, 2, 3, 4, 5, 6, 7].map(type => ({ value: String(type), label: t(`logs:logType_${type}`) }))} />
              </Field>
            </div>
          </div>
        </CardContent>
      </form>
  )
}
