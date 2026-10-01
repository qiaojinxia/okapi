import { Check, Scale, X } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import type { PricingModel, TokenUnit } from './types'
import { capabilityKeys, formatContextWindow, modelCapabilities, modelPrice, modelVendor, nonnegative } from './catalog-data'
import type { PriceField } from './catalog-data'
import { ModelAvailability, ModelId } from './ModelCatalogItem'
import { VendorIcon } from './VendorIcon'
import { MAX_COMPARE } from './CompareToggle'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Drawer } from '@/components/ui/drawer'
import { formatUnitPrice } from '@/lib/money'
import { cn } from '@/lib/utils'

/// 底部托盘：选中的模型（可逐个移出）、计数、清空与"开始对比"。至少 2 个才能开始。
export function CompareTray({ models, onRemove, onClear, onStart }: {
  models: PricingModel[]; onRemove: (model: string) => void; onClear: () => void; onStart: () => void
}) {
  const { t } = useTranslation()
  return <div role="region" aria-label={t('catalog:compareTrayLabel')}
    className="fixed inset-x-0 bottom-0 z-30 border-t border-border bg-card/95 shadow-[0_-8px_24px_-14px_rgb(0_0_0/0.25)] backdrop-blur-md">
    <div className="mx-auto flex max-w-[1480px] flex-wrap items-center gap-x-4 gap-y-2 px-4 py-3 sm:px-8">
      <span className="text-sm font-medium tabular-nums">{t('catalog:compareTray', { n: models.length, max: MAX_COMPARE })}</span>
      {/* 窄屏：第一行计数 + 操作，第二行芯片单行横滑，不再把托盘撑成三四行高 */}
      <ul className="order-3 flex w-full min-w-0 gap-2 overflow-x-auto sm:order-none sm:w-auto sm:flex-1 sm:flex-wrap sm:overflow-visible">
        {models.map((model) => {
          const name = model.display_name || model.model
          return <li key={model.model} className="inline-flex max-w-56 shrink-0 items-center gap-1.5 rounded-lg border border-border bg-background py-1 pl-1.5 pr-1 text-xs">
            <VendorIcon vendor={modelVendor(model)} size="sm" />
            <span className="truncate font-medium" title={name}>{name}</span>
            <button type="button" onClick={() => onRemove(model.model)} aria-label={t('catalog:compareRemoveFor', { model: name })}
              className="flex h-6 w-6 shrink-0 items-center justify-center rounded-md text-muted-foreground outline-none hover:bg-muted hover:text-foreground focus-visible:ring-2 focus-visible:ring-primary/40"><X aria-hidden className="h-3.5 w-3.5" /></button>
          </li>
        })}
      </ul>
      {models.length < 2 && <span className="order-4 w-full text-xs text-muted-foreground sm:order-none sm:w-auto">{t('catalog:compareNeedTwo')}</span>}
      <div className="order-2 ml-auto flex items-center gap-2 sm:order-none">
        <Button variant="ghost" size="sm" onClick={onClear}>{t('catalog:compareClear')}</Button>
        <Button size="sm" disabled={models.length < 2} onClick={onStart}><Scale className="h-3.5 w-3.5" />{t('catalog:compareStart')}</Button>
      </div>
    </div>
  </div>
}

type Cell = { node: React.ReactNode; value?: number | null }
interface Row { key: string; label: string; best?: 'low' | 'high'; cells: Cell[] }

/// 并排对比抽屉：价格、规格、能力、接入情况逐行对齐；数值行把同行最优的格子标成绿色（至少两个有值且不全相同）。
export function CompareDrawer({ models, group, factor, unit, onRemove, onClose }: {
  models: PricingModel[]; group: string; factor: number | null; unit: TokenUnit
  onRemove: (model: string) => void; onClose: () => void
}) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const dash = <span className="text-muted-foreground">—</span>
  const price = (field: PriceField, label: string): Row => ({
    key: field, label: `${label} / ${unit}`, best: 'low',
    cells: models.map((model) => {
      const micro = model.mode === 'ratio' ? modelPrice(model, field, factor, unit) : null
      return { value: micro, node: model.mode === 'tiered' && field === 'input' ? t('catalog:variablePrice') : micro === null ? dash : formatUnitPrice(micro, locale) }
    }),
  })
  const rows: Row[] = [
    { key: 'mode', label: t('catalog:billingMode'), cells: models.map((model) => ({ node: t(`analysis:${model.mode}`, { defaultValue: t('catalog:customPricing') }) })) },
    price('input', t('pricing:promptPrice')), price('output', t('pricing:completionPrice')),
    price('cache', t('pricing:cachedPrice')), price('cacheWrite', t('pricing:cacheWritePrice')),
    ...(models.some((model) => model.mode === 'per_call') ? [{
      key: 'call', label: t('catalog:comparePerRequest'), best: 'low' as const,
      cells: models.map((model) => {
        const micro = model.mode === 'per_call' ? modelPrice(model, 'call', factor) : null
        return { value: micro, node: micro === null ? dash : `${formatUnitPrice(micro, locale)} ${t('catalog:perRequest')}` }
      }),
    }] : []),
    ...([['context', t('catalog:context'), 'context_window'], ['maxOutput', t('catalog:maxOutput'), 'max_output']] as const).map(([key, label, field]): Row => ({
      key, label, best: 'high',
      cells: models.map((model) => {
        const value = nonnegative(model[field])
        return { value: value && value > 0 ? value : null, node: value && value > 0 ? <span title={`${value.toLocaleString(locale)} tokens`}>{formatContextWindow(value)}</span> : dash }
      }),
    })),
    ...capabilityKeys.filter((cap) => models.some((model) => modelCapabilities(model).includes(cap))).map((cap): Row => ({
      key: `cap:${cap}`, label: t(`catalog:cap_${cap}`),
      cells: models.map((model) => ({ node: modelCapabilities(model).includes(cap)
        ? <span className="inline-flex items-center gap-1 text-success"><Check aria-hidden className="h-4 w-4" /><span className="sr-only">{t('catalog:compareSupported')}</span></span>
        : <span className="text-muted-foreground" title={t('catalog:compareUnsupported')}>—</span> })),
    })),
    { key: 'availability', label: t('catalog:availability'), cells: models.map((model) => ({ node: <ModelAvailability model={model} group={group} /> })) },
  ]
  // 同行最优：至少两个有值且不全相同才标——全部相同或只有一个值时标"最优"没有意义。
  const bestIndexes = (row: Row) => {
    const values = row.cells.map((cell) => cell.value ?? null)
    const known = values.filter((value): value is number => value !== null)
    if (!row.best || known.length < 2) return new Set<number>()
    const target = row.best === 'low' ? Math.min(...known) : Math.max(...known)
    if (known.every((value) => value === target)) return new Set<number>()
    return new Set(values.flatMap((value, index) => value === target ? [index] : []))
  }
  return <Drawer open onClose={onClose} title={t('catalog:compareTitle')} description={t('catalog:compareHint')} size="xl">
    <div className="overflow-x-auto">
      {/* 定宽布局：列宽均分，模型名 / ID 在列内截断，而不是被最长的那列撑出横向滚动、把最后一列的"移出"按钮挤到视口外 */}
      <table aria-label={t('catalog:compareTable')} style={{ minWidth: `${8 + models.length * 10}rem` }} className="w-full table-fixed border-separate border-spacing-0 text-sm">
        <thead>
          <tr>
            <th scope="col" className="sticky left-0 z-10 w-32 bg-card" />
            {models.map((model) => <th key={model.model} scope="col" className="border-b border-border px-3 pb-4 text-left align-top font-normal">
              {/* 图标与"移出"同一行、名称与 ID 在其下占满整列：四列并排时名字不再被截成 "Claude So..." */}
              <div className="flex items-center justify-between">
                <VendorIcon vendor={modelVendor(model)} size="sm" />
                <button type="button" onClick={() => onRemove(model.model)} aria-label={t('catalog:compareRemoveFor', { model: model.display_name || model.model })}
                  className="-mr-1 flex h-7 w-7 shrink-0 items-center justify-center rounded-md text-muted-foreground outline-none hover:bg-muted hover:text-foreground focus-visible:ring-2 focus-visible:ring-primary/40"><X aria-hidden className="h-4 w-4" /></button>
              </div>
              <p className="mt-2 truncate text-xs text-muted-foreground">{modelVendor(model).name || t('catalog:otherVendor')}</p>
              <p className="text-sm font-semibold leading-snug [overflow-wrap:anywhere]">{model.display_name || model.model}</p>
              <ModelId model={model} />
            </th>)}
          </tr>
        </thead>
        <tbody>
          {rows.map((row) => {
            const best = bestIndexes(row)
            return <tr key={row.key} className="group">
              <th scope="row" className="sticky left-0 z-10 border-b border-border/60 bg-card py-3 pr-3 text-left text-xs font-normal leading-snug text-muted-foreground">{row.label}</th>
              {row.cells.map((cell, index) => <td key={models[index].model}
                className={cn('border-b border-border/60 px-3 py-3 tabular-nums', best.has(index) && 'font-semibold text-success')}
                title={best.has(index) ? t(row.best === 'low' ? 'catalog:compareLowest' : 'catalog:compareHighest') : undefined}>
                {cell.node}{best.has(index) && <Badge variant="success" className="ml-2 align-middle">{t(row.best === 'low' ? 'catalog:compareLowest' : 'catalog:compareHighest')}</Badge>}
              </td>)}
            </tr>
          })}
        </tbody>
      </table>
    </div>
  </Drawer>
}
