import { useId, useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { TFunction } from 'i18next'
import { ChevronDown, SlidersHorizontal, X } from 'lucide-react'
import type { AnalyticsSearch } from '@/features/analytics/route-state'
import { Button } from '@/components/ui/button'
import { DateRangeField } from '@/components/ui/date-range'
import { Drawer } from '@/components/ui/drawer'
import { Input } from '@/components/ui/input'
import { ModelInput } from '@/features/models/model-input'
import { ADVANCED_KEYS } from '@/lib/cube'
export { ADVANCED_KEYS } from '@/lib/cube'

export function dimensionLabel(t: TFunction, dim: string): string {
  const basic: Record<string, string> = { model: 'dimModel', channel: 'dimChannel', provider: 'dimProvider', user: 'dimUser', api_key: 'dimApiKey', group: 'dimGroup' }
  return basic[dim] ? t(`analytics:${basic[dim]}`) : t(`analysis:${dim}`)
}
export const selectClass = 'h-9 w-full min-w-0 rounded-lg border border-border bg-card px-2 text-sm text-foreground focus-visible:outline-2 focus-visible:outline-primary'

function Choices({ label, values, onChange, model = false }: { label: string; values: string[]; onChange: (v: string[]) => void; model?: boolean }) {
  const { t } = useTranslation()
  const [input, setInput] = useState('')
  const id = useId()
  const add = (text = input) => { const v = text.trim(); if (v && v.length <= 256 && values.length < 8 && !values.includes(v)) { onChange([...values, v]); setInput('') } }
  const disabled = values.length >= 8
  return <div className="min-w-0 space-y-2">
    <label htmlFor={id} className="text-xs text-muted-foreground">{label}</label>
    <div className="flex min-w-0 gap-2">
      {model ? <ModelInput id={id} value={input} maxLength={256} disabled={disabled} onChange={setInput} onChoose={add} onSubmit={() => add()} className="min-w-0 flex-1" placeholder={t('admin:pickModelsSearch')} />
        : <Input id={id} value={input} maxLength={256} disabled={disabled} className="min-w-0 flex-1" onChange={(e) => setInput(e.target.value)} onKeyDown={(e) => { if (e.key === 'Enter') { e.preventDefault(); if (!e.nativeEvent.isComposing) add() } }} placeholder={t('analysis:enterValue')} />}
      <Button variant="outline" onClick={() => add()} disabled={!input.trim() || disabled || values.includes(input.trim())}>{t('analytics:addFilter')}</Button>
    </div>
    <div className="flex max-h-28 flex-wrap gap-1 overflow-y-auto overscroll-contain">
      {values.map((v) => <span key={v} className="inline-flex max-w-full items-center gap-1 rounded-full bg-primary/10 py-0.5 pr-0.5 pl-2 text-xs text-primary">
        <span className="min-w-0 truncate" title={v}>{v}</span>
        <button type="button" aria-label={t('analytics:removeFilter', { name: v })} className="flex h-7 w-7 shrink-0 items-center justify-center rounded-full outline-none hover:bg-primary/15 focus-visible:ring-2 focus-visible:ring-primary/40" onClick={() => onChange(values.filter((x) => x !== v))}><X size={14} /></button>
      </span>)}
    </div>
  </div>
}

export function AnalysisControls({ value, onApply, today }: { value: AnalyticsSearch; onApply: (next: AnalyticsSearch) => void; today?: string }) {
  const { t } = useTranslation()
  const [open, setOpen] = useState(false)
  const key = JSON.stringify([value.days, ...ADVANCED_KEYS.map((k) => value[k])])
  const label = (k: typeof ADVANCED_KEYS[number]) => t(`analysis:${k === 'stream' ? 'request_type' : k}`)
  const display = (k: typeof ADVANCED_KEYS[number]) => { const v = value[k]; return Array.isArray(v) ? v.join(' · ') : k === 'stream' && typeof v === 'boolean' ? t(v ? 'analysis:stream' : 'analysis:non_stream') : ['granularity', 'request_type', 'billing_type'].includes(k) && typeof v === 'string' && ['hour', 'day', 'stream', 'non_stream', 'websocket', 'ratio', 'tiered', 'per_call'].includes(v) ? t(`analysis:${v}`) : String(v) }
  const active = ADVANCED_KEYS.filter((k) => value[k] !== undefined && !(Array.isArray(value[k]) && !value[k]?.length))
  const chips: { id: string; text: string; primary?: boolean }[] = []
  if (value.start_date) chips.push({ id: 'range', text: `${value.start_date} — ${value.end_date}`, primary: true })
  if (value.model_source) chips.push({ id: 'source', text: t(`analysis:source_${value.model_source}`), primary: true })
  for (const k of active.filter((key) => !['start_date', 'end_date', 'model_source'].includes(key))) {
    chips.push({ id: k, text: `${label(k)}: ${display(k)}` })
  }
  const apply = (next: AnalyticsSearch) => { onApply(next); setOpen(false) }
  return <>
    <section data-slot="analysis-filter-summary" aria-label={t('analysis:advanced')} className="flex min-h-11 min-w-0 items-center gap-2 overflow-hidden rounded-xl border border-border bg-card px-2 py-1.5">
      <Button variant="ghost" size="sm" aria-haspopup="dialog" aria-expanded={open} onClick={() => setOpen(true)} className="shrink-0 px-2">
        <SlidersHorizontal aria-hidden size={16} className="text-muted-foreground" />
        <span>{t('analysis:advanced')}</span>
        {active.length > 0 && <span className="rounded-md bg-muted px-1.5 py-0.5 text-[11px] text-muted-foreground" aria-label={t('analysis:active', { n: active.length })}>{active.length}</span>}
        <ChevronDown aria-hidden size={15} className="text-muted-foreground" />
      </Button>
      {chips.length === 0 ? (
        <span className="min-w-0 flex-1 truncate text-xs text-muted-foreground">{t('analysis:advancedHint')}</span>
      ) : (
        <div className="flex min-w-0 flex-1 items-center gap-1.5 overflow-x-auto whitespace-nowrap scrollbar-none" aria-label={t('analysis:active', { n: active.length })}>
          {chips.slice(0, 3).map((chip) => <span key={chip.id} className={`shrink-0 rounded-md px-2 py-1 text-xs ${chip.primary ? 'bg-primary/8 text-primary' : 'bg-muted text-foreground'}`} title={chip.text}>{chip.text}</span>)}
          {chips.length > 3 && <button type="button" className="min-h-7 shrink-0 rounded-md px-2 text-xs text-muted-foreground hover:bg-muted hover:text-foreground" onClick={() => setOpen(true)}>+{chips.length - 3}</button>}
        </div>
      )}
    </section>
    <Editor key={key} open={open} value={value} onApply={apply} onClose={() => setOpen(false)} today={today} />
  </>
}

function Editor({ open, value, onApply, onClose, today }: { open: boolean; value: AnalyticsSearch; onApply: (next: AnalyticsSearch) => void; onClose: () => void; today?: string }) {
  const { t } = useTranslation()
  const [draft, setDraft] = useState(value)
  const [error, setError] = useState(false)
  const calendarToday = today ?? new Date().toISOString().slice(0, 10)
  const patch = (p: Partial<AnalyticsSearch>) => { setDraft((s) => ({ ...s, ...p })); setError(false) }
  const apply = () => {
    const { start_date: start, end_date: end } = draft
    const days = start && end ? (Date.parse(end) - Date.parse(start)) / 86400_000 + 1 : draft.days ?? 7
    if (!!start !== !!end || !Number.isFinite(days) || days < 1 || days > 366 || (end && today && end > today) || (draft.granularity === 'hour' && days > 31)) { setError(true); return }
    const next = { ...value }; for (const k of ADVANCED_KEYS) Object.assign(next, { [k]: draft[k] })
    if (draft.models?.length) next.model = undefined
    if (draft.groups?.length) next.group = undefined
    onApply(next)
  }
  const reset = () => {
    const next = { ...value }; for (const k of ADVANCED_KEYS) delete next[k]
    onApply(next)
  }
  const field = (name: 'endpoint' | 'upstream_endpoint' | 'node') => <label key={name} className="min-w-0 space-y-1 text-xs text-muted-foreground">{t(`analysis:${name}`)}<Input value={draft[name] ?? ''} maxLength={256} onChange={(e) => patch({ [name]: e.target.value.trim() || undefined })} /></label>
  return <Drawer open={open} onClose={onClose} title={t('analysis:advanced')} description={t('analysis:advancedHint')} size="lg" footer={<>
    <Button variant="outline" onClick={reset}>{t('analysis:reset')}</Button>
    <Button variant="ghost" onClick={onClose}>{t('common:close')}</Button>
    <Button onClick={apply}>{t('analysis:apply')}</Button>
  </>}>
    <form className="space-y-5" onSubmit={(e) => { e.preventDefault(); apply() }}>
      {error && <p role="alert" className="text-sm text-destructive">{t('analysis:invalidRange')}</p>}
      <div className="space-y-5" role="region" aria-label={t('analysis:editConditions')} tabIndex={0}>
        <fieldset className="min-w-0 space-y-3">
          <legend className="text-sm font-medium">{t('analysis:timeSection')}</legend>
          <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
            <div className="col-span-2 min-w-0 space-y-1">
              <span className="block text-xs text-muted-foreground">{t('analysis:dateRange')}</span>
              <DateRangeField today={calendarToday} label={t('analysis:dateRange')} placeholder={t('analysis:dateRangePlaceholder')} value={{ start: draft.start_date ?? '', end: draft.end_date ?? '' }} onChange={(range) => patch({ start_date: range.start || undefined, end_date: range.end || undefined })} />
            </div>
            <label className="space-y-1 text-xs text-muted-foreground">{t('analysis:granularity')}<select aria-label={t('analysis:granularity')} className={selectClass} value={draft.granularity ?? ''} onChange={(e) => patch({ granularity: e.target.value as AnalyticsSearch['granularity'] || undefined })}>{['', 'hour', 'day'].map((v) => <option key={v} value={v}>{t(`analysis:${v || 'auto'}`)}</option>)}</select></label>
            <label className="space-y-1 text-xs text-muted-foreground">{t('analysis:model_source')}<select aria-label={t('analysis:model_source')} className={selectClass} value={draft.model_source ?? 'billed'} onChange={(e) => patch({ model_source: e.target.value as AnalyticsSearch['model_source'] })}>{['billed', 'requested', 'upstream'].map((v) => <option key={v} value={v}>{t(`analysis:source_${v}`)}</option>)}</select></label>
          </div>
          <p className="text-xs leading-5 text-muted-foreground">{t('analysis:rangeHint')}</p>
        </fieldset>
        <fieldset className="min-w-0 space-y-3 border-t border-border pt-3">
          <legend className="pr-2 text-sm font-medium">{t('analysis:requestSection')}</legend>
          <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 xl:grid-cols-3">
            {field('endpoint')}{field('upstream_endpoint')}{field('node')}
            <label className="space-y-1 text-xs text-muted-foreground">{t('analysis:request_type')}<select aria-label={t('analysis:request_type')} className={selectClass} value={draft.request_type ?? (draft.stream === true ? 'stream' : draft.stream === false ? 'non_stream' : '')} onChange={(e) => patch({ request_type: e.target.value || undefined, stream: undefined })}>{['', 'stream', 'non_stream', 'websocket'].map((v) => <option key={v} value={v}>{t(`analysis:${v || 'all'}`)}</option>)}</select></label>
            <label className="space-y-1 text-xs text-muted-foreground">{t('analysis:billing_type')}<select aria-label={t('analysis:billing_type')} className={selectClass} value={draft.billing_type ?? ''} onChange={(e) => patch({ billing_type: e.target.value || undefined })}>{['', 'ratio', 'tiered', 'per_call'].map((v) => <option key={v} value={v}>{t(`analysis:${v || 'all'}`)}</option>)}{draft.billing_type && !['ratio', 'tiered', 'per_call'].includes(draft.billing_type) && <option value={draft.billing_type}>{draft.billing_type}</option>}</select></label>
          </div>
        </fieldset>
        <fieldset className="min-w-0 space-y-3 border-t border-border pt-3">
          <legend className="pr-2 text-sm font-medium">{t('analysis:comparisonSection')}</legend>
          <div className="grid gap-4 md:grid-cols-2"><Choices model label={t('analysis:models')} values={draft.models ?? []} onChange={(v) => patch({ models: v.length ? v : undefined })} /><Choices label={t('analysis:groups')} values={draft.groups ?? []} onChange={(v) => patch({ groups: v.length ? v : undefined })} /></div>
          <p className="text-xs leading-5 text-muted-foreground">{t('analysis:selectionHint')}</p>
        </fieldset>
      </div>
    </form>
  </Drawer>
}
