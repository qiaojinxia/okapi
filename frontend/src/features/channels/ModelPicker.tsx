import { useQuery } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import { ErrorState } from '@/components/ui/state'
import { Label } from '@/components/ui/input'
import { SearchInput } from '@/components/ui/search-input'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { qk } from '@/lib/query-keys'
import { VendorIcon } from '@/features/public-pricing/VendorIcon'
import { modelVendor } from '@/features/public-pricing/catalog-data'
import { cn } from '@/lib/utils'

export interface PickerModel {
  model_name: string
  display_name?: string | null
  vendor: string | null
  pricing_mode: string | null
}


/// 模型选择器：从**已配定价**的模型里勾选，避免手输拼错——模型名拼错的后果是
/// 请求直接 404 且不易排查。
///
/// 按供应商分组展示（vendor 由后端按模型名前缀自动归类）；未定价模型标红，
/// 因为建了渠道却没定价，请求同样会被拒。
export function ModelPicker({
  value,
  onChange,
}: {
  value: string[]
  onChange: (models: string[]) => void
}) {
  const { t } = useTranslation()
  const [search, setSearch] = useState('')
  const [selectedOnly, setSelectedOnly] = useState(false)
  const models = useQuery({
    queryKey: qk.adminModels,
    queryFn: () => apiFetch<{ data: PickerModel[] }>('/admin/models'),
  })

  const picked = new Set(value)
  const toggle = (name: string) => {
    const next = new Set(picked)
    if (next.has(name)) next.delete(name)
    else next.add(name)
    onChange([...next])
  }

  const catalog = models.data?.data ?? []
  const registered = new Set(catalog.map((model) => model.model_name))
  // 手动添加的模型也属于“已选”，不能在查看选择结果时漏掉。
  const candidates: PickerModel[] = selectedOnly ? [
    ...catalog,
    ...value.filter((name) => !registered.has(name)).map((model_name) => ({ model_name, vendor: null, pricing_mode: null })),
  ] : catalog
  const terms = search.normalize('NFKC').toLowerCase().trim().split(/\s+/).filter(Boolean)
  const matched = candidates.filter((m) => {
    const text = `${m.model_name} ${m.display_name ?? ''} ${m.vendor ?? ''}`.normalize('NFKC').toLowerCase()
    return (!selectedOnly || picked.has(m.model_name)) && terms.every((term) => text.includes(term))
  })
  const matchedNames = new Set(matched.map((m) => m.model_name))
  const groups = new Map<string, PickerModel[]>()
  for (const m of matched) {
    const key = m.vendor ?? ''
    const list = groups.get(key) ?? []
    list.push(m)
    groups.set(key, list)
  }
  // 未归类的排到末尾
  const ordered = [...groups.entries()].sort(([a], [b]) => {
    if (a === '') return 1
    if (b === '') return -1
    return a.localeCompare(b)
  })

  if (models.isError) {
    return <ErrorState message={describeError(models.error)} />
  }
  return (
    <div className="flex min-w-0 flex-col gap-3 rounded-xl border border-border bg-muted/20 p-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <Label>{t('admin:pickModels')}</Label>
        <Badge variant="default" aria-live="polite">{t('common:selectedCount', { n: value.length })}</Badge>
      </div>
      <SearchInput value={search} onChange={setSearch} placeholder={t('admin:pickModelsSearch')} aria-label={t('admin:pickModels')} />
      <div className="flex flex-wrap items-center justify-between gap-2 border-b border-border pb-2">
        <Button size="sm" variant={selectedOnly ? 'default' : 'ghost'} aria-pressed={selectedOnly}
          onClick={() => setSelectedOnly(!selectedOnly)}>{t('admin:onlySelectedModels')}</Button>
        <div className="flex flex-wrap gap-1">
          <Button size="sm" variant="ghost" disabled={matched.every((m) => picked.has(m.model_name))}
            onClick={() => onChange([...new Set([...value, ...matchedNames])])}>{t('admin:selectMatchedModels')}</Button>
          <Button size="sm" variant="ghost" disabled={!matched.some((m) => picked.has(m.model_name))}
            onClick={() => onChange(value.filter((name) => !matchedNames.has(name)))}>{t('admin:clearMatchedModels')}</Button>
        </div>
      </div>
      <div className="flex max-h-72 min-w-0 flex-col gap-4 overflow-y-auto overscroll-contain pr-1 [scrollbar-gutter:stable]">
        {ordered.length === 0 ? (
          <span className="px-2 py-4 text-center text-xs leading-5 text-muted-foreground">{t(models.isPending ? 'common:loading' : search.trim() ? 'common:noResults' : selectedOnly ? 'admin:pickSelectedEmpty' : 'admin:pickModelsEmpty')}</span>
        ) : (
          ordered.map(([vendor, list]) => (
            <div key={vendor || 'other'} className="flex min-w-0 flex-col gap-2">
              <div className="flex min-w-0 items-center gap-2">
                <VendorIcon vendor={modelVendor({ vendor })} size="sm" />
                <span className="min-w-0 flex-1 break-words text-xs font-semibold">{vendor || t('admin:vendorOther')}</span>
                <span className="text-xs text-muted-foreground">{list.length}</span>
              </div>
              <div className="grid min-w-0 grid-cols-1 gap-2 sm:grid-cols-2">
                {list.map((m) => (
                  <Checkbox
                    key={m.model_name}
                    label={m.model_name}
                    checked={picked.has(m.model_name)}
                    onChange={() => toggle(m.model_name)}
                    className={cn('min-h-14 min-w-0 rounded-lg border bg-card px-3 py-2 font-mono transition-colors hover:border-primary/40 focus-within:ring-2 focus-within:ring-primary/25', picked.has(m.model_name) ? 'border-primary/40 bg-primary/5' : 'border-border')}
                    description={<>
                      {m.display_name && m.display_name !== m.model_name && <span className="break-words">{m.display_name}</span>}
                      {m.pricing_mode === null && <Badge variant="destructive">{t('admin:unpriced')}</Badge>}
                    </>}
                  />
                ))}
              </div>
            </div>
          ))
        )}
      </div>
      <span className="text-xs text-muted-foreground" aria-live="polite">{t('common:resultCount', { n: matched.length })}</span>
    </div>
  )
}
