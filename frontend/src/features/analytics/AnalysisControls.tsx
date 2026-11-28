import { useId, useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { TFunction } from 'i18next'
import { SlidersHorizontal, X } from 'lucide-react'
import type { AnalyticsSearch } from '@/features/analytics/route-state'
import { Alert } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { DateRangeField } from '@/components/ui/date-range'
import { Drawer, FieldGroup } from '@/components/ui/drawer'
import { Input } from '@/components/ui/input'
import { Select } from '@/components/ui/select'
import { ModelInput } from '@/features/models/model-input'
import { ADVANCED_KEYS } from '@/lib/cube'
import { cn } from '@/lib/utils'
export { ADVANCED_KEYS } from '@/lib/cube'

export function dimensionLabel(t: TFunction, dim: string): string {
  const basic: Record<string, string> = { model: 'dimModel', channel: 'dimChannel', provider: 'dimProvider', user: 'dimUser', api_key: 'dimApiKey', group: 'dimGroup' }
  return basic[dim] ? t(`analytics:${basic[dim]}`) : t(`analysis:${dim}`)
}

function Choices({ label, values, onChange, model = false }: { label: string; values: string[]; onChange: (v: string[]) => void; model?: boolean }) {
  const { t } = useTranslation()
  const [input, setInput] = useState('')
  const id = useId()
  const add = (text = input) => { const v = text.trim(); if (v && v.length <= 256 && values.length < 8 && !values.includes(v)) { onChange([...values, v]); setInput('') } }
  const disabled = values.length >= 8
  return <div className="min-w-0 space-y-2">
    <label htmlFor={id} className="block text-xs font-medium text-foreground/85">{label}</label>
    <div className="flex min-w-0 gap-2">
      {model ? <ModelInput id={id} value={input} maxLength={256} disabled={disabled} onChange={setInput} onChoose={add} onSubmit={() => add()} className="min-w-0 flex-1" placeholder={t('admin:pickModelsSearch')} />
        : <Input id={id} value={input} maxLength={256} disabled={disabled} className="min-w-0 flex-1" onChange={(e) => setInput(e.target.value)} onKeyDown={(e) => { if (e.key === 'Enter') { e.preventDefault(); if (!e.nativeEvent.isComposing) add() } }} placeholder={t('analysis:enterValue')} />}
      <Button variant="outline" onClick={() => add()} disabled={!input.trim() || disabled || values.includes(input.trim())}>{t('analytics:addFilter')}</Button>
    </div>
    {values.length > 0 && <div className="flex max-h-28 flex-wrap gap-1.5 overflow-y-auto overscroll-contain pt-0.5">
      {values.map((v) => <span key={v} className="inline-flex max-w-full items-center gap-1 rounded-lg bg-primary/10 py-0.5 pr-1 pl-2.5 text-xs font-medium text-primary ring-1 ring-inset ring-primary/15">
        <span className="min-w-0 truncate" title={v}>{v}</span>
        <button type="button" aria-label={t('analytics:removeFilter', { name: v })} className="flex h-5 w-5 shrink-0 cursor-pointer items-center justify-center rounded-md outline-none transition-colors hover:bg-primary/15 focus-visible:ring-2 focus-visible:ring-primary/40" onClick={() => onChange(values.filter((x) => x !== v))}><X className="h-3 w-3" /></button>
      </span>)}
    </div>}
  </div>
}

/// 高级条件的入口：一个按钮 + 生效条件的标签，和快速筛选排在同一行；条件本身在抽屉里编辑，不占页面高度。
export function AnalysisControls({ value, onApply, today, className }: { value: AnalyticsSearch; onApply: (next: AnalyticsSearch) => void; today?: string; className?: string }) {
  const { t } = useTranslation()
  const [open, setOpen] = useState(false)
  const key = JSON.stringify([value.days, ...ADVANCED_KEYS.map((k) => value[k])])
  const label = (k: typeof ADVANCED_KEYS[number]) => t(`analysis:${k === 'stream' ? 'request_type' : k}`)
  const display = (k: typeof ADVANCED_KEYS[number]) => { const v = value[k]; return Array.isArray(v) ? v.join(' · ') : k === 'stream' && typeof v === 'boolean' ? t(v ? 'analysis:stream' : 'analysis:non_stream') : ['granularity', 'request_type', 'billing_type'].includes(k) && typeof v === 'string' && ['hour', 'day', 'stream', 'non_stream', 'websocket', 'ratio', 'tiered', 'per_call'].includes(v) ? t(`analysis:${v}`) : String(v) }
  const active = ADVANCED_KEYS.filter((k) => value[k] !== undefined && !(Array.isArray(value[k]) && !value[k]?.length))
  const clearKeys = (keys: (typeof ADVANCED_KEYS[number])[]) => {
    const next = { ...value }
    for (const k of keys) delete next[k]
    onApply(next)
  }
  const chips: { id: string; text: string; primary?: boolean; removeKeys: (typeof ADVANCED_KEYS[number])[] }[] = []
  if (value.start_date) chips.push({ id: 'range', text: `${value.start_date} — ${value.end_date}`, primary: true, removeKeys: ['start_date', 'end_date'] })
  if (value.model_source) chips.push({ id: 'source', text: t(`analysis:source_${value.model_source}`), primary: true, removeKeys: ['model_source'] })
  for (const k of active.filter((key) => !['start_date', 'end_date', 'model_source'].includes(key))) {
    chips.push({ id: k, text: `${label(k)}: ${display(k)}`, removeKeys: k === 'request_type' || k === 'stream' ? ['request_type', 'stream'] : [k] })
  }
  const apply = (next: AnalyticsSearch) => { onApply(next); setOpen(false) }
  return <>
    <section data-slot="analysis-filter-summary" aria-label={t('analysis:advanced')} className={cn('flex min-h-9 min-w-0 max-w-full flex-wrap items-center gap-2', className)}>
      <Button variant="outline" size="sm" aria-haspopup="dialog" aria-expanded={open} title={t('analysis:advancedHint')} onClick={() => setOpen(true)} className="min-h-11 shrink-0 md:min-h-9">
        <SlidersHorizontal aria-hidden className="h-3.5 w-3.5 text-muted-foreground" />
        <span>{t('analysis:advanced')}</span>
        {active.length > 0 && <span className="rounded-full bg-primary/12 px-1.5 text-[11px] leading-[18px] font-medium text-primary tabular-nums" aria-label={t('analysis:active', { n: active.length })}>{active.length}</span>}
      </Button>
      {/* 生效的条件全部列出来（多了换行）：点文字直接进抽屉改，点 × 即刻撤掉这一项 */}
      {chips.map((chip) => (
        <span
          key={chip.id}
          className={cn(
            'inline-flex h-7 min-w-0 max-w-full items-center gap-1 rounded-full pr-1 pl-2.5 text-xs ring-1 ring-inset',
            chip.primary
              ? 'bg-primary/10 font-medium text-primary ring-primary/20'
              : 'bg-card text-foreground shadow-xs ring-border/80',
          )}
          title={chip.text}
        >
          <button
            type="button"
            onClick={() => setOpen(true)}
            className="min-w-0 cursor-pointer truncate rounded-sm text-left outline-none hover:underline hover:underline-offset-2 focus-visible:ring-2 focus-visible:ring-primary/40"
          >
            {chip.text}
          </button>
          <button
            type="button"
            aria-label={t('analytics:removeFilter', { name: chip.text })}
            onClick={() => clearKeys(chip.removeKeys)}
            className={cn(
              'flex h-5 w-5 shrink-0 cursor-pointer items-center justify-center rounded-full outline-none transition-colors focus-visible:ring-2 focus-visible:ring-primary/40',
              chip.primary ? 'hover:bg-primary/15' : 'text-muted-foreground hover:bg-muted hover:text-foreground',
            )}
          >
            <X className="h-3 w-3" />
          </button>
        </span>
      ))}
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
  const field = (name: 'endpoint' | 'upstream_endpoint' | 'node') => <label key={name} className="flex min-w-0 flex-col gap-1.5 text-xs font-medium text-foreground/85">{t(`analysis:${name}`)}<Input value={draft[name] ?? ''} maxLength={256} onChange={(e) => patch({ [name]: e.target.value.trim() || undefined })} /></label>
  return <Drawer open={open} onClose={onClose} title={t('analysis:advanced')} description={t('analysis:advancedHint')} size="lg" footer={<>
    <Button variant="outline" onClick={reset}>{t('analysis:reset')}</Button>
    <Button variant="ghost" onClick={onClose}>{t('common:close')}</Button>
    <Button onClick={apply}>{t('analysis:apply')}</Button>
  </>}>
    <form className="flex flex-col gap-4" onSubmit={(e) => { e.preventDefault(); apply() }}>
      {error && <Alert tone="destructive">{t('analysis:invalidRange')}</Alert>}
      <div className="form-sections" role="region" aria-label={t('analysis:editConditions')} tabIndex={0}>
        <FieldGroup title={t('analysis:timeSection')} hint={t('analysis:rangeHint')}>
          <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
            <div className="min-w-0 space-y-1.5 sm:col-span-2">
              <span className="block text-xs font-medium text-foreground/85">{t('analysis:dateRange')}</span>
              <DateRangeField today={calendarToday} label={t('analysis:dateRange')} placeholder={t('analysis:dateRangePlaceholder')} value={{ start: draft.start_date ?? '', end: draft.end_date ?? '' }} onChange={(range) => patch({ start_date: range.start || undefined, end_date: range.end || undefined })} />
            </div>
            <label className="flex flex-col gap-1.5 text-xs font-medium text-foreground/85">
              {t('analysis:granularity')}
              <Select
                aria-label={t('analysis:granularity')}
                className="w-full"
                value={draft.granularity ?? ''}
                onChange={(v) => patch({ granularity: (v as AnalyticsSearch['granularity']) || undefined })}
                options={['', 'hour', 'day'].map((v) => ({ value: v, label: t(`analysis:${v || 'auto'}`) }))}
              />
            </label>
            <label className="flex flex-col gap-1.5 text-xs font-medium text-foreground/85">
              {t('analysis:model_source')}
              <Select
                aria-label={t('analysis:model_source')}
                className="w-full"
                value={draft.model_source ?? 'billed'}
                onChange={(v) => patch({ model_source: v as AnalyticsSearch['model_source'] })}
                options={['billed', 'requested', 'upstream'].map((v) => ({ value: v, label: t(`analysis:source_${v}`) }))}
              />
            </label>
          </div>
        </FieldGroup>
        <FieldGroup title={t('analysis:requestSection')}>
          <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 xl:grid-cols-3">
            {field('endpoint')}{field('upstream_endpoint')}{field('node')}
            <label className="flex flex-col gap-1.5 text-xs font-medium text-foreground/85">
              {t('analysis:request_type')}
              <Select
                aria-label={t('analysis:request_type')}
                className="w-full"
                value={draft.request_type ?? (draft.stream === true ? 'stream' : draft.stream === false ? 'non_stream' : '')}
                onChange={(v) => patch({ request_type: v || undefined, stream: undefined })}
                options={['', 'stream', 'non_stream', 'websocket'].map((v) => ({ value: v, label: t(`analysis:${v || 'all'}`) }))}
              />
            </label>
            <label className="flex flex-col gap-1.5 text-xs font-medium text-foreground/85">
              {t('analysis:billing_type')}
              <Select
                aria-label={t('analysis:billing_type')}
                className="w-full"
                value={draft.billing_type ?? ''}
                onChange={(v) => patch({ billing_type: v || undefined })}
                options={[
                  ...['', 'ratio', 'tiered', 'per_call'].map((v) => ({ value: v, label: t(`analysis:${v || 'all'}`) })),
                  ...(draft.billing_type && !['ratio', 'tiered', 'per_call'].includes(draft.billing_type)
                    ? [{ value: draft.billing_type, label: draft.billing_type }]
                    : []),
                ]}
              />
            </label>
          </div>
        </FieldGroup>
        <FieldGroup title={t('analysis:comparisonSection')} hint={t('analysis:selectionHint')}>
          <div className="grid gap-4 md:grid-cols-2">
            <Choices model label={t('analysis:models')} values={draft.models ?? []} onChange={(v) => patch({ models: v.length ? v : undefined })} />
            <Choices label={t('analysis:groups')} values={draft.groups ?? []} onChange={(v) => patch({ groups: v.length ? v : undefined })} />
          </div>
        </FieldGroup>
      </div>
    </form>
  </Drawer>
}
