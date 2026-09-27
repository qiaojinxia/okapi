import { useId, useLayoutEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { TFunction } from 'i18next'
import { ChevronDown, SlidersHorizontal, X } from 'lucide-react'
import type { AnalyticsSearch } from '@/routes/admin.stats'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { ModelInput } from '@/features/models/model-input'
import { ADVANCED_KEYS } from './advanced-search'
export { ADVANCED_KEYS } from './advanced-search'

export function dimensionLabel(t: TFunction, dim: string): string {
  const basic: Record<string, string> = { model: 'dimModel', channel: 'dimChannel', provider: 'dimProvider', user: 'dimUser', api_key: 'dimApiKey', group: 'dimGroup' }
  return basic[dim] ? t(`analytics:${basic[dim]}`) : t(`analysis:${dim}`)
}
export const selectClass = 'h-9 w-full min-w-0 rounded-md border border-border bg-card px-2 text-sm text-foreground focus-visible:outline-2 focus-visible:outline-primary'

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
  // Remount the editor only when applied filters change; view/metric switches retain drafts.
  const key = JSON.stringify([value.days, ...ADVANCED_KEYS.map((k) => value[k])])
  const label = (k: typeof ADVANCED_KEYS[number]) => t(`analysis:${k === 'stream' ? 'request_type' : k}`)
  const display = (k: typeof ADVANCED_KEYS[number]) => { const v = value[k]; return Array.isArray(v) ? v.join(' · ') : k === 'stream' && typeof v === 'boolean' ? t(v ? 'analysis:stream' : 'analysis:non_stream') : ['granularity', 'request_type', 'billing_type'].includes(k) && typeof v === 'string' && ['hour', 'day', 'stream', 'non_stream', 'websocket', 'ratio', 'tiered', 'per_call'].includes(v) ? t(`analysis:${v}`) : String(v) }
  const active = ADVANCED_KEYS.filter((k) => value[k] !== undefined && !(Array.isArray(value[k]) && !value[k]?.length))
  return <details className="group min-w-0 rounded-xl border border-border bg-card">
    <summary className="flex min-h-11 cursor-pointer flex-wrap items-center gap-2 rounded-xl px-3 py-2.5 text-sm outline-none marker:content-none focus-visible:ring-2 focus-visible:ring-primary/40">
      <SlidersHorizontal size={16} className="shrink-0 text-muted-foreground" /><span className="font-medium">{t('analysis:advanced')}</span>
      <span className="min-w-0 flex-1 text-xs text-muted-foreground">{active.length ? t('analysis:active', { n: active.length }) : t('analysis:advancedHint')}</span>
      <ChevronDown aria-hidden size={16} className="shrink-0 text-muted-foreground transition-transform group-open:rotate-180" />
      {active.length > 0 && <span className="flex w-full min-w-0 flex-wrap gap-1.5">
        {value.start_date && <span className="rounded bg-primary/8 px-2 py-1 text-xs text-primary">{value.start_date} — {value.end_date}</span>}
        {value.model_source && <span className="rounded bg-primary/8 px-2 py-1 text-xs text-primary">{t(`analysis:source_${value.model_source}`)}</span>}
        {active.filter((k) => !['start_date', 'end_date', 'model_source'].includes(k)).map((k) => <span key={k} className="max-w-full truncate rounded bg-muted px-2 py-1 text-xs sm:max-w-72" title={`${label(k)}: ${display(k)}`}>{label(k)}: {display(k)}</span>)}
      </span>}
    </summary>
    <Editor key={key} value={value} onApply={onApply} today={today} />
  </details>
}
function Editor({ value, onApply, today }: { value: AnalyticsSearch; onApply: (next: AnalyticsSearch) => void; today?: string }) {
  const { t } = useTranslation()
  const [draft, setDraft] = useState(value)
  const [error, setError] = useState(false)
  const form = useRef<HTMLFormElement>(null)
  const fields = useRef<HTMLDivElement>(null)
  const actions = useRef<HTMLDivElement>(null)
  // 用实际剩余视口约束表单，操作栏占自己的行，不覆盖正在编辑的字段。
  useLayoutEffect(() => {
    const root = form.current, body = fields.current, footer = actions.current
    if (!root || !body || !footer) return
    let frame = 0
    const measure = () => {
      if (!root.getClientRects().length) return
      const viewport = window.visualViewport
      const bottom = (viewport?.offsetTop ?? 0) + (viewport?.height ?? window.innerHeight)
      const available = Math.max(160, Math.floor(bottom - Math.max(0, root.getBoundingClientRect().top) - footer.offsetHeight - 16))
      const height = `min(55dvh, 32rem, ${available}px)`
      if (body.style.maxHeight !== height) body.style.maxHeight = height
    }
    const schedule = () => { cancelAnimationFrame(frame); frame = requestAnimationFrame(measure) }
    const observer = new ResizeObserver(schedule)
    observer.observe(root)
    observer.observe(footer)
    if (root.parentElement) observer.observe(root.parentElement)
    window.addEventListener('resize', schedule)
    window.addEventListener('scroll', schedule, { passive: true })
    window.visualViewport?.addEventListener('resize', schedule)
    window.visualViewport?.addEventListener('scroll', schedule)
    schedule()
    return () => {
      cancelAnimationFrame(frame)
      observer.disconnect()
      window.removeEventListener('resize', schedule)
      window.removeEventListener('scroll', schedule)
      window.visualViewport?.removeEventListener('resize', schedule)
      window.visualViewport?.removeEventListener('scroll', schedule)
    }
  }, [])
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
  const field = (name: 'endpoint' | 'upstream_endpoint' | 'node') => <label key={name} className="min-w-0 space-y-1 text-xs text-muted-foreground">{t(`analysis:${name}`)}<Input value={draft[name] ?? ''} maxLength={256} onChange={(e) => patch({ [name]: e.target.value.trim() || undefined })} /></label>
  return <form ref={form} className="min-w-0 border-t border-border" onSubmit={(e) => { e.preventDefault(); apply() }}>
    <div ref={fields} className="max-h-[min(55dvh,32rem)] space-y-5 overflow-y-auto overscroll-contain p-4" role="region" aria-label={t('analysis:editConditions')} tabIndex={0}>
    <fieldset className="min-w-0 space-y-3">
      <legend className="text-sm font-medium">{t('analysis:timeSection')}</legend>
      <div className="grid grid-cols-2 gap-3 xl:grid-cols-4">
      {(['start_date', 'end_date'] as const).map((name) => <label key={name} className="min-w-0 space-y-1 text-xs text-muted-foreground">{t(name === 'start_date' ? 'charts:range_start' : 'charts:range_end')}<Input type="date" className="min-w-0" min="1971-01-01" max={today} value={draft[name] ?? ''} onChange={(e) => patch({ [name]: e.target.value || undefined })} /></label>)}
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
    <div ref={actions} className="space-y-2 rounded-b-xl border-t border-border bg-card px-4 py-3">
      {error && <p role="alert" className="text-sm text-destructive">{t('analysis:invalidRange')}</p>}
      <div className="flex flex-wrap items-center gap-2 [&>button]:min-h-10 max-sm:[&>button]:flex-1"><Button type="submit">{t('analysis:apply')}</Button><Button variant="outline" onClick={() => { const next = { ...value }; for (const k of ADVANCED_KEYS) delete next[k]; onApply(next); setDraft(next); setError(false) }}>{t('analysis:reset')}</Button></div>
    </div>
  </form>
}
