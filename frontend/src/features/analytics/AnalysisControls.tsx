import { useId, useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { TFunction } from 'i18next'
import { SlidersHorizontal, X } from 'lucide-react'
import type { AnalyticsSearch } from '@/routes/admin.stats'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'

export function dimensionLabel(t: TFunction, dim: string): string {
  const basic: Record<string, string> = { model: 'dimModel', channel: 'dimChannel', provider: 'dimProvider', user: 'dimUser', api_key: 'dimApiKey', group: 'dimGroup' }
  return basic[dim] ? t(`analytics:${basic[dim]}`) : t(`analysis:${dim}`)
}
export const ADVANCED_KEYS = ['start_date', 'end_date', 'granularity', 'model_source', 'endpoint', 'upstream_endpoint', 'node', 'request_type', 'billing_type', 'stream', 'models', 'groups'] as const
export const selectClass = 'h-9 w-full min-w-0 rounded-md border border-border bg-card px-2 text-sm text-foreground focus-visible:outline-2 focus-visible:outline-primary'

function Choices({ label, values, onChange }: { label: string; values: string[]; onChange: (v: string[]) => void }) {
  const { t } = useTranslation()
  const [input, setInput] = useState('')
  const id = useId()
  const add = () => { const v = input.trim(); if (v && v.length <= 256 && values.length < 8 && !values.includes(v)) { onChange([...values, v]); setInput('') } }
  return <div className="space-y-2"><label htmlFor={id} className="text-xs text-muted-foreground">{label}</label><div className="flex gap-2"><Input id={id} value={input} maxLength={256} disabled={values.length >= 8} onChange={(e) => setInput(e.target.value)} onKeyDown={(e) => { if (e.key === 'Enter') { e.preventDefault(); add() } }} placeholder={t('analysis:enterValue')} /><Button variant="outline" onClick={add} disabled={!input.trim() || values.length >= 8}>{t('analytics:addFilter')}</Button></div><div className="flex flex-wrap gap-1">{values.map((v) => <span key={v} className="inline-flex max-w-full items-center gap-1 rounded-full bg-primary/10 py-1 pr-1 pl-2 text-xs text-primary"><span className="truncate" title={v}>{v}</span><button type="button" aria-label={t('analytics:removeFilter', { name: v })} className="rounded-full p-1 hover:bg-primary/15" onClick={() => onChange(values.filter((x) => x !== v))}><X size={14} /></button></span>)}</div></div>
}

export function AnalysisControls({ value, onApply, today }: { value: AnalyticsSearch; onApply: (next: AnalyticsSearch) => void; today?: string }) {
  const { t } = useTranslation()
  // Remount the editor only when applied filters change; view/metric switches retain drafts.
  const key = JSON.stringify(ADVANCED_KEYS.map((k) => value[k]))
  const display = (k: typeof ADVANCED_KEYS[number]) => { const v = value[k]; return Array.isArray(v) ? v.join(' · ') : ['granularity', 'request_type', 'billing_type'].includes(k) && typeof v === 'string' && ['hour', 'day', 'stream', 'non_stream', 'websocket', 'ratio', 'tiered', 'per_call'].includes(v) ? t(`analysis:${v}`) : String(v) }
  const active = ADVANCED_KEYS.filter((k) => value[k] !== undefined && !(Array.isArray(value[k]) && !value[k]?.length))
  return <details className="rounded-xl border border-border bg-card">
    <summary className="flex min-h-11 cursor-pointer flex-wrap items-center gap-2 px-4 py-3 text-sm marker:content-none"><SlidersHorizontal size={16} className="text-muted-foreground" /><span className="font-medium">{t('analysis:advanced')}</span><span className="text-xs text-muted-foreground">{active.length ? t('analysis:active', { n: active.length }) : t('analysis:advancedHint')}</span>{value.start_date && <span className="text-xs text-primary">{value.start_date} — {value.end_date}</span>}{value.model_source && <span className="text-xs text-primary">{t(`analysis:source_${value.model_source}`)}</span>}{active.filter((k) => !['start_date', 'end_date', 'model_source'].includes(k)).map((k) => <span key={k} className="max-w-56 truncate rounded bg-muted px-2 py-0.5 text-xs" title={String(value[k])}>{t(`analysis:${k}`)}: {display(k)}</span>)}</summary>
    <Editor key={key} value={value} onApply={onApply} today={today} />
  </details>
}
function Editor({ value, onApply, today }: { value: AnalyticsSearch; onApply: (next: AnalyticsSearch) => void; today?: string }) {
  const { t } = useTranslation()
  const [draft, setDraft] = useState(value)
  const [error, setError] = useState(false)
  const patch = (p: Partial<AnalyticsSearch>) => { setDraft((s) => ({ ...s, ...p })); setError(false) }
  const apply = () => {
    const { start_date: start, end_date: end } = draft
    const days = start && end ? (Date.parse(end) - Date.parse(start)) / 86400_000 + 1 : draft.days ?? 7
    if (!!start !== !!end || !Number.isFinite(days) || days < 1 || days > 366 || (end && today && end > today) || (draft.granularity === 'hour' && days > 31)) { setError(true); return }
    // Model lists replace a previous single-model focus; selected source applies to all views.
    const next = { ...value }; for (const k of ADVANCED_KEYS) Object.assign(next, { [k]: draft[k] })
    if (draft.models?.length) next.model = undefined
    if (draft.groups?.length) next.group = undefined
    onApply(next)
  }
  const field = (name: 'endpoint' | 'upstream_endpoint' | 'node' | 'billing_type') => <label key={name} className="space-y-1 text-xs text-muted-foreground">{t(`analysis:${name}`)}<Input value={draft[name] ?? ''} maxLength={256} onChange={(e) => patch({ [name]: e.target.value.trim() || undefined })} /></label>
  return <form className="space-y-4 border-t border-border p-4" onSubmit={(e) => { e.preventDefault(); apply() }}>
    <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 xl:grid-cols-4">
      {(['start_date', 'end_date'] as const).map((name) => <label key={name} className="space-y-1 text-xs text-muted-foreground">{t(name === 'start_date' ? 'charts:range_start' : 'charts:range_end')}<Input type="date" min="1971-01-01" max={today} value={draft[name] ?? ''} onChange={(e) => patch({ [name]: e.target.value || undefined })} /></label>)}
      <label className="space-y-1 text-xs text-muted-foreground">{t('analysis:granularity')}<select aria-label={t('analysis:granularity')} className={selectClass} value={draft.granularity ?? ''} onChange={(e) => patch({ granularity: e.target.value as AnalyticsSearch['granularity'] || undefined })}>{['', 'hour', 'day'].map((v) => <option key={v} value={v}>{t(`analysis:${v || 'auto'}`)}</option>)}</select></label>
      <label className="space-y-1 text-xs text-muted-foreground">{t('analysis:model_source')}<select aria-label={t('analysis:model_source')} className={selectClass} value={draft.model_source ?? 'billed'} onChange={(e) => patch({ model_source: e.target.value as AnalyticsSearch['model_source'] })}>{['billed', 'requested', 'upstream'].map((v) => <option key={v} value={v}>{t(`analysis:source_${v}`)}</option>)}</select></label>
      {field('endpoint')}{field('upstream_endpoint')}{field('node')}
      <label className="space-y-1 text-xs text-muted-foreground">{t('analysis:request_type')}<select aria-label={t('analysis:request_type')} className={selectClass} value={draft.request_type ?? ''} onChange={(e) => patch({ request_type: e.target.value || undefined, stream: undefined })}>{['', 'stream', 'non_stream', 'websocket'].map((v) => <option key={v} value={v}>{t(`analysis:${v || 'all'}`)}</option>)}</select></label>
      <label className="space-y-1 text-xs text-muted-foreground">{t('analysis:billing_type')}<select aria-label={t('analysis:billing_type')} className={selectClass} value={draft.billing_type ?? ''} onChange={(e) => patch({ billing_type: e.target.value || undefined })}>{['', 'ratio', 'tiered', 'per_call'].map((v) => <option key={v} value={v}>{t(`analysis:${v || 'all'}`)}</option>)}{draft.billing_type && !['ratio', 'tiered', 'per_call'].includes(draft.billing_type) && <option value={draft.billing_type}>{draft.billing_type}</option>}</select></label>
    </div>
    <div className="grid gap-4 md:grid-cols-2"><Choices label={t('analysis:models')} values={draft.models ?? []} onChange={(v) => patch({ models: v.length ? v : undefined })} /><Choices label={t('analysis:groups')} values={draft.groups ?? []} onChange={(v) => patch({ groups: v.length ? v : undefined })} /></div>
    <p className="text-xs text-muted-foreground">{t('analysis:selectionHint')}</p>
    {error && <p role="alert" className="text-sm text-destructive">{t('analysis:invalidRange')}</p>}
    <div className="flex flex-wrap items-center gap-2"><Button type="submit">{t('analysis:apply')}</Button><Button variant="ghost" onClick={() => { const next = { ...value }; for (const k of ADVANCED_KEYS) delete next[k]; onApply(next); setDraft(next); setError(false) }}>{t('analysis:reset')}</Button><span className="text-xs text-muted-foreground">{t('analysis:rangeHint')}</span></div>
  </form>
}
