import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { PricingModel } from './types'
import { modelPrice, nonnegative } from './catalog-data'
import { Input, Label } from '@/components/ui/input'
import { formatUnitPrice } from '@/lib/money'

// 跟随当前模型和分组，避免用户在两个独立选择器之间重复操作。
export function Simulator({ model, groupFactor }: { model: PricingModel; groupFactor: number | null }) {
  const { t, i18n } = useTranslation()
  const [form, setForm] = useState({ prompt: '1000', cached: '0', cacheWrite: '0', cache_write_5m: '0', cache_write_1h: '0', completion: '500', calls: '1' })
  const ttl = model.modality_ratios?.cache_write_5m != null || model.modality_ratios?.cache_write_1h != null
  const fields = model.mode === 'per_call'
    ? [['calls', t('catalog:simCalls')]] as const
    : [['prompt', t('pricing:simPrompt')], ['completion', t('pricing:simCompletion')],
      ['cached', t('pricing:simCached')], ['cacheWrite', t('pricing:simCacheWrite')],
      ...(ttl ? [['cache_write_5m', t('logs:cacheWrite5m')], ['cache_write_1h', t('logs:cacheWrite1h')]] as const : [])] as const
  const values = Object.fromEntries(Object.entries(form).map(([key, value]) => [key, nonnegative(value)]))
  const invalid = fields.some(([key]) => values[key] === null || !Number.isSafeInteger(values[key]))
    || (model.mode === 'ratio' && values.cached! + values.cacheWrite! + (ttl ? values.cache_write_5m! + values.cache_write_1h! : 0) > values.prompt!)
  let estimate: number | null = null
  if (!invalid) {
    if (model.mode === 'per_call') {
      const price = modelPrice(model, 'call', groupFactor)
      estimate = price === null ? null : Math.round(price * values.calls!)
    } else if (model.mode === 'ratio') {
      const parts = [
        ['input', values.prompt! - values.cached! - values.cacheWrite! - (ttl ? values.cache_write_5m! + values.cache_write_1h! : 0)], ['output', values.completion!],
        ['cache', values.cached!], ['cacheWrite', values.cacheWrite!],
        ...(ttl ? [['cache_write_5m', values.cache_write_5m!], ['cache_write_1h', values.cache_write_1h!]] as const : []),
      ] as const
      const costs = parts.map(([field, tokens]) => {
        const price = modelPrice(model, field, groupFactor)
        return tokens === 0 ? 0 : price === null ? null : price * tokens / 1_000_000
      })
      if (costs.every((cost) => cost !== null)) estimate = Math.round(costs.reduce((sum, cost) => sum + cost, 0))
    }
  }
  return <section className="rounded-xl border border-border bg-muted/25 p-4">
    <h3 className="text-sm font-semibold">{t('pricing:simulator')}</h3>
    <p className="mt-1 text-xs leading-5 text-muted-foreground">{t('catalog:simContext')}</p>
    {ttl && <p className="mt-1 text-xs leading-5 text-muted-foreground">{t('admin:modelMeta.simTtlHint')}</p>}
    <div className="mt-4 grid grid-cols-2 gap-3">
      {fields.map(([key, label]) => <div key={key} className="flex flex-col gap-1.5">
        <Label htmlFor={`sim-${key}`}>{label}</Label>
        <Input id={`sim-${key}`} type="number" min={0} step={1} inputMode="numeric" value={form[key]}
          onChange={(e) => setForm((old) => ({ ...old, [key]: e.target.value }))} />
      </div>)}
    </div>
    {invalid && <p role="alert" className="mt-3 text-xs text-destructive">{t('catalog:simInvalid')}</p>}
    <div className="mt-4 flex items-baseline justify-between gap-2 border-t border-border pt-3" aria-live="polite">
      <span className="text-xs text-muted-foreground">{t('pricing:simResult')}</span>
      <strong className="text-xl tracking-tight">{formatUnitPrice(estimate, i18n.language)}</strong>
    </div>
    <p className="mt-2 text-xs leading-5 text-muted-foreground">{t('pricing:simNote')}</p>
  </section>
}
