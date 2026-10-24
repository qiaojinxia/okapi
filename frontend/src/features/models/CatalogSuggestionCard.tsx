import { ExternalLink } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { CAPABILITY_KEYS } from './types'
import { NOTE_CODES, type CatalogSnapshot, type CatalogSuggestion } from './catalog'

const SHOWN_CAPABILITIES = ['vision', 'tools', 'reasoning', 'audio', 'video'] as const
const NOTE_PARAMS = ['above_tokens', 'input_per_1m', 'output_per_1m', 'tiers', 'input_usd', 'output_usd'] as const

/// 内置价目目录的建议卡：价格、缓存倍率、规格和需要留意的计价条件。
/// 它只是参考——两个按钮都只改表单草稿，保存并发布之后价格才生效。
export function CatalogSuggestionCard({ suggestion, snapshot, canApplyPrice, onApplyPrice, onApplySpecs }: {
  suggestion: CatalogSuggestion
  snapshot: CatalogSnapshot
  /// 当前计价方式能不能接这条建议（按倍率 / 按次）。
  canApplyPrice: boolean
  onApplyPrice: () => void
  onApplySpecs: () => void
}) {
  const { t, i18n } = useTranslation()
  const count = (value: number) => value.toLocaleString(i18n.language)
  const s = suggestion
  const today = new Date().toISOString().slice(0, 10)
  const cache = [
    s.cache_read_ratio && t('admin:modelCatalog.cacheRead', { value: s.cache_read_ratio }),
    s.cache_write_ratio && t('admin:modelCatalog.cacheWrite', { value: s.cache_write_ratio }),
    s.cache_write_1h_ratio && t('admin:modelCatalog.cacheWrite1h', { value: s.cache_write_1h_ratio }),
  ].filter(Boolean).join(' · ')
  const limits = [
    s.context_window !== null && t('admin:modelCatalog.limitsContext', { context: count(s.context_window) }),
    s.max_output !== null && t('admin:modelCatalog.limitsOutput', { output: count(s.max_output) }),
  ].filter(Boolean).join(' · ')
  const badges = [
    s.kind ? t(`admin:modelKinds.${s.kind}`) : '',
    ...SHOWN_CAPABILITIES.filter((key) => CAPABILITY_KEYS.includes(key) && s.capabilities[key] === true)
      .map((key) => t(`admin:modelCaps.${key}`)),
  ].filter(Boolean)
  const priced = s.billing === 'ratio' && s.input_per_1m !== null
  const perCall = s.billing === 'per_call' && s.per_call_usd !== null

  return <div className="space-y-2 rounded-lg border border-border bg-muted/40 px-3 py-2.5" data-testid="model-catalog-suggestion">
    <div className="flex flex-wrap items-center justify-between gap-2 text-xs">
      <span className="font-medium">{t('admin:modelCatalog.title')}
        <span className="ml-1.5 font-normal text-muted-foreground">{t('admin:modelCatalog.unverified')}</span></span>
      <span className="inline-flex items-center gap-2 text-muted-foreground">
        {t('admin:modelCatalog.snapshot', { date: snapshot.generated_at })}
        {s.source_url && <a className="inline-flex items-center gap-1 hover:text-primary" href={s.source_url} target="_blank" rel="noopener noreferrer">
          {t('admin:modelCatalog.source')}<ExternalLink aria-hidden className="h-3 w-3" /></a>}
      </span>
    </div>
    <p className="break-all font-mono text-xs text-muted-foreground">{t('admin:modelCatalog.matched', { key: s.key, provider: s.provider })}</p>
    {badges.length > 0 && <div className="flex flex-wrap gap-1.5">
      {badges.map((label) => <span key={label} className="rounded bg-background/80 px-1.5 py-0.5 text-xs text-muted-foreground">{label}</span>)}
    </div>}
    {(priced || perCall) && <div className="flex flex-wrap items-center justify-between gap-2 border-t border-border/70 pt-2">
      <div className="min-w-0 space-y-0.5 text-xs">
        <p className="font-medium tabular-nums" data-testid="model-catalog-price">
          {priced
            ? t(s.output_per_1m === null ? 'admin:modelCatalog.priceLineInput' : 'admin:modelCatalog.priceLine', { input: s.input_per_1m, output: s.output_per_1m })
            : t('admin:modelCatalog.perCall', { price: s.per_call_usd })}
        </p>
        {priced && cache && <p className="text-muted-foreground tabular-nums">{cache}</p>}
      </div>
      <Button size="sm" variant="outline" disabled={!canApplyPrice} title={canApplyPrice ? undefined : t('admin:modelCatalog.needsRatio')}
        onClick={onApplyPrice}>
        {t(perCall ? 'admin:modelCatalog.applyPerCall' : 'admin:modelCatalog.applyPrice')}
      </Button>
    </div>}
    {s.billing === 'unsupported' && <p className="border-t border-border/70 pt-2 text-xs leading-5 text-muted-foreground">{t('admin:modelCatalog.unsupported')}</p>}
    {limits && <div className="flex flex-wrap items-center justify-between gap-2 text-xs text-muted-foreground">
      <span className="tabular-nums">{limits}</span>
      <Button size="sm" variant="ghost" onClick={onApplySpecs}>{t('admin:modelCatalog.applySpecs')}</Button>
    </div>}
    {!limits && <div className="flex justify-end">
      <Button size="sm" variant="ghost" onClick={onApplySpecs}>{t('admin:modelCatalog.applySpecs')}</Button>
    </div>}
    {s.deprecated && <p role="status" className="text-xs leading-5 text-amber-600 dark:text-amber-400">
      {t(s.deprecated <= today ? 'admin:modelCatalog.deprecatedPast' : 'admin:modelCatalog.deprecatedFuture', { date: s.deprecated })}</p>}
    {s.notes.filter((note) => NOTE_CODES.includes(note.code)).map((note) => <p key={`${note.code}-${note.params?.above_tokens ?? ''}`} className="text-xs leading-5 text-muted-foreground">
      {t(`admin:modelCatalog.notes.${note.code}`, Object.fromEntries(NOTE_PARAMS.map((name) => [name, note.params?.[name] ?? '—'])))}
    </p>)}
    {s.alternatives.length > 0 && <details className="text-xs leading-5 text-muted-foreground">
      <summary className="cursor-pointer text-primary">{t('admin:modelCatalog.alternatives')} {s.alternatives.length}</summary>
      <ul className="mt-1 space-y-0.5 font-mono">
        {s.alternatives.map((alternative) => <li key={alternative.key} className="break-all">
          {alternative.key} · {alternative.provider}
          {alternative.input_per_1m !== null && <span> · ${alternative.input_per_1m}{alternative.output_per_1m !== null && ` / $${alternative.output_per_1m}`}</span>}
        </li>)}
      </ul>
    </details>}
    <p className="text-[11px] leading-4 text-muted-foreground">{t('admin:modelCatalog.scope')}</p>
  </div>
}
