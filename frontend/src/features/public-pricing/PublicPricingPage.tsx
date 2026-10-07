import { getRouteApi, Link } from '@tanstack/react-router'
import { ArrowRight, Boxes, ChevronDown, LayoutGrid, List, Moon, SlidersHorizontal, Sun, X } from 'lucide-react'
import { useEffect, useMemo, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { CatalogSearch, PricingGroup, TokenUnit } from './types'
import { usePagedCatalog } from './use-paged-catalog'
import { catalogVendors } from './paged-catalog'
import { capabilityKeys, nonnegative } from './catalog-data'
import { VendorIcon } from './VendorIcon'
import { ModelCard, ModelTableRow } from './ModelCatalogItem'
import { ModelDetails } from './ModelDetails'
import { MAX_COMPARE } from './CompareToggle'
import { CompareDrawer, CompareTray } from './ModelCompare'
import { Pagination } from '@/components/ui/pagination'
import { BrandLockup } from '@/components/brand'
import { PoweredBy } from '@/components/powered-by'
import { Button, buttonVariants } from '@/components/ui/button'
import { Label } from '@/components/ui/input'
import { SearchInput } from '@/components/ui/search-input'
import { Segmented } from '@/components/ui/segmented'
import { Select } from '@/components/ui/select'
import { EmptyState, ErrorState, LoadingState } from '@/components/ui/state'
import { TBody, THead, Table, Th, Tr } from '@/components/ui/table'
import { getKey } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatRatio } from '@/lib/money'
import { useTheme } from '@/lib/theme'
import { cn } from '@/lib/utils'

const route = getRouteApi('/pricing')

export function PublicPricingPage() {
  const { t, i18n } = useTranslation()
  const theme = useTheme()
  const signedIn = getKey() !== null
  const search = route.useSearch()
  const vendorNav = useRef<HTMLElement>(null)
  const [filtersOpen, setFiltersOpen] = useState(false)
  const resultsHeading = useRef<HTMLHeadingElement>(null)
  const navigate = route.useNavigate()
  const patch = (values: Partial<CatalogSearch>, replace = true) => {
    const resetPage = ['q', 'vendor', 'group', 'mode', 'capability', 'available', 'sort', 'pageSize'].some((key) => key in values)
    void navigate({ search: (old) => ({ ...old, ...(resetPage ? { page: undefined } : {}), ...values }), replace, resetScroll: false })
  }
  const unit: TokenUnit = search.unit ?? '1M'
  const catalog = usePagedCatalog(search)
  const pricing = { ...catalog.page,
    isSuccess: catalog.page.isSuccess && catalog.stats.isSuccess && catalog.mine.isSuccess,
    isPending: catalog.page.isPending || catalog.stats.isPending || catalog.mine.isPending,
    isError: catalog.page.isError || catalog.stats.isError || catalog.mine.isError,
    error: catalog.mine.error ?? catalog.page.error ?? catalog.stats.error,
    refetch: () => catalog.mine.isError ? catalog.mine.refetch() : Promise.all([catalog.page.refetch(), catalog.stats.refetch()]),
  }
  const groups = pricing.isSuccess ? pricing.data?.groups ?? [] : []
  const shown = pricing.isSuccess ? pricing.data?.models ?? [] : []
  const group = groups.some((g) => g.code === search.group) ? search.group! : ''
  const total = pricing.data?.total ?? 0
  const totalModels = catalog.stats.data?.total ?? 0
  const pageSize = search.pageSize ?? 24
  const offset = ((search.page ?? 1) - 1) * pageSize
  const clampedPage = Math.min(search.page ?? 1, Math.max(1, Math.ceil(total / pageSize)))
  const outOfRange = pricing.isSuccess && clampedPage !== (search.page ?? 1)
  useEffect(() => {
    if (!pricing.isSuccess) return
    if (search.group && !groups.some((g) => g.code === search.group)) {
      void navigate({ search: (old) => ({ ...old, group: undefined, page: undefined }), replace: true, resetScroll: false })
    } else if (outOfRange) {
      void navigate({ search: (old) => ({ ...old, page: clampedPage === 1 ? undefined : clampedPage }), replace: true, resetScroll: false })
    }
  }, [pricing.isSuccess, groups, search.group, outOfRange, clampedPage, navigate])
  const groupInfo = groups.find((g) => g.code === group)
  const factor = group ? nonnegative(groupInfo?.ratio) : 1
  const query = search.q?.trim() ?? ''
  const vendors = useMemo(() => catalogVendors(catalog.stats.data), [catalog.stats.data])
  const capabilities = capabilityKeys.filter((cap) => catalog.stats.data?.capabilities.includes(cap))
  const changePage = (offset: number) => {
    patch({ page: offset === 0 ? undefined : offset / pageSize + 1 }, false)
    resultsHeading.current?.focus({ preventScroll: true })
    resultsHeading.current?.scrollIntoView({ block: 'start' })
  }
  const selected = catalog.models.get(search.model ?? '') ?? shown.find((m) => m.model === search.model)
  const detailQuery = catalog.detailQuery
  const lookupError = catalog.details.find((query) => query.isError)
  // 对比列表以 URL 为准；目录里已不存在的 ID（分享链接过期、模型下架）直接忽略，不占名额。
  const compared = (search.compare?.split(',') ?? []).flatMap((id) => {
    const model = catalog.models.get(id) ?? shown.find((m) => m.model === id)
    return model ? [model] : []
  }).slice(0, MAX_COMPARE)
  const comparedIds = (search.compare?.split(',') ?? []).filter((id) => !catalog.missing.has(id))
  const setCompare = (ids: string[], extra: Partial<CatalogSearch> = {}) => patch({ compare: ids.length ? ids.join(',') : undefined, ...(ids.length < 2 ? { comparing: undefined } : {}), ...extra })
  const toggleCompare = (id: string) => comparedIds.includes(id) ? setCompare(comparedIds.filter((x) => x !== id)) : comparedIds.length < MAX_COMPARE ? setCompare([...comparedIds, id]) : undefined
  const compareProps = (model: { model: string }) => ({ compared: comparedIds.includes(model.model), compareFull: comparedIds.length >= MAX_COMPARE, onCompare: () => toggleCompare(model.model) })
  const active = !!(query || search.vendor || search.mode || search.capability || search.available)
  const clear = () => patch({ q: undefined, vendor: undefined, mode: undefined, capability: undefined, available: undefined })
  const groupName = (g: PricingGroup) => g.name || (g.code === 'default' ? t('flow:defaultGroup') : g.code)
  const activeVendor = vendors.find((v) => v.vendor.id === search.vendor)?.vendor
  useEffect(() => {
    const nav = vendorNav.current
    const selected = nav?.querySelector<HTMLElement>('[aria-pressed="true"]')
    if (!nav || !selected || nav.scrollWidth <= nav.clientWidth) return
    const parent = nav.getBoundingClientRect(), child = selected.getBoundingClientRect()
    if (child.left < parent.left || child.right > parent.right) nav.scrollLeft += child.left - parent.left - (nav.clientWidth - child.width) / 2
  }, [search.vendor, vendors.length])

  return <div className="min-h-screen bg-background">
    <header className="sticky top-0 z-20 border-b border-border bg-card/95 backdrop-blur-md">
      <div className="mx-auto flex h-16 max-w-[1480px] items-center justify-between px-4 sm:px-8">
        <div className="flex items-center gap-6"><Link to="/" className="rounded outline-none focus-visible:ring-2 focus-visible:ring-primary/40"><BrandLockup /></Link>
          <span className="hidden border-l border-border pl-6 text-sm font-medium text-muted-foreground sm:block">{t('pricing:title')}</span></div>
        <div className="flex items-center gap-2">
          <Button variant="ghost" size="icon" className="h-9 w-9" aria-label={t('common:theme')} onClick={theme.toggle}>{theme.resolved === 'dark' ? <Sun className="h-4 w-4" /> : <Moon className="h-4 w-4" />}</Button>
          <Link to={signedIn ? '/portal' : '/'} className={buttonVariants({ size: 'sm' })}>{signedIn ? t('common:portal') : t('common:login')}<ArrowRight className="h-3.5 w-3.5" /></Link>
        </div>
      </div>
    </header>
    <main className={cn('mx-auto w-full max-w-[1480px] px-4 pb-12 sm:px-8', compared.length > 0 && 'pb-32')}>
      <div className="flex flex-wrap items-end justify-between gap-3 py-6 sm:gap-5 sm:py-10">
        <div><div className="mb-3 hidden items-center gap-2 text-xs font-semibold tracking-[0.14em] text-primary sm:flex"><Boxes className="h-4 w-4" />{t('catalog:eyebrow')}</div>
          <h1 className="text-2xl font-semibold tracking-tight sm:text-4xl">{t('pricing:title')}</h1>
          <p className="mt-3 hidden text-sm leading-6 text-muted-foreground sm:block">{t('catalog:subtitle')}</p></div>
        {catalog.stats.isSuccess && <p className="flex items-baseline gap-1.5 text-xs text-muted-foreground sm:gap-2 sm:text-sm"><strong className="font-semibold text-foreground sm:text-xl">{totalModels.toLocaleString(i18n.language)}</strong>{t('catalog:modelsUnit')}<span className="mx-1 text-border sm:mx-2">/</span><strong className="font-semibold text-foreground sm:text-xl">{vendors.filter((v) => v.vendor.id !== 'other').length}</strong>{t('catalog:vendorsUnit')}</p>}
      </div>
      <div className="flex flex-col items-start gap-6 lg:flex-row lg:gap-8">
        <aside className="w-full shrink-0 lg:sticky lg:top-24 lg:w-52">
          <div className="mb-3 flex items-center justify-between px-1"><h2 className="text-xs font-semibold text-muted-foreground">{t('catalog:vendors')}</h2><span className="text-xs text-muted-foreground lg:hidden">{t('catalog:swipeVendors')}</span></div>
          <nav ref={vendorNav} aria-label={t('catalog:vendors')} className="flex gap-1.5 overflow-x-auto pb-2 lg:max-h-[calc(100vh-180px)] lg:flex-col lg:overflow-y-auto lg:pb-0">
            <button type="button" aria-pressed={!search.vendor} onClick={() => patch({ vendor: undefined })}
              className={cn('flex min-h-11 shrink-0 items-center gap-2.5 rounded-xl px-3 text-sm outline-none focus-visible:ring-2 focus-visible:ring-primary/40', !search.vendor ? 'bg-primary/10 font-semibold text-primary' : 'hover:bg-muted')}>
              <Boxes className="m-1.5 h-5 w-5" /><span className="flex-1 whitespace-nowrap text-left">{t('catalog:allVendors')}</span><span className="text-xs opacity-70">{catalog.stats.isSuccess ? totalModels : '—'}</span>
            </button>
            {vendors.map(({ vendor, count }) => <button key={vendor.id} type="button" aria-pressed={search.vendor === vendor.id} onClick={() => patch({ vendor: vendor.id })}
              className={cn('flex min-h-11 shrink-0 items-center gap-2.5 rounded-xl px-3 py-1.5 text-sm outline-none focus-visible:ring-2 focus-visible:ring-primary/40', search.vendor === vendor.id ? 'bg-primary/10 font-semibold text-primary' : 'text-muted-foreground hover:bg-muted hover:text-foreground')}>
              <VendorIcon vendor={vendor} size="sm" /><span className="min-w-0 flex-1 truncate text-left" title={vendor.name}>{vendor.name || t('catalog:otherVendor')}</span><span className="text-xs opacity-70">{count}</span>
            </button>)}
          </nav>
        </aside>
        <div className="min-w-0 w-full flex-1">
          <SearchInput className="w-full max-w-xl" value={search.q ?? ''} onChange={(q) => patch({ q: q || undefined })} aria-label={t('pricing:searchHint')} placeholder={t('catalog:searchPlaceholder')} inputClassName="h-12 rounded-xl pl-10 text-sm" />
          <button type="button" aria-expanded={filtersOpen} aria-controls="catalog-filters" onClick={() => setFiltersOpen((open) => !open)}
            className="mt-3 flex min-h-11 w-full items-center gap-2 rounded-lg border border-border bg-card px-3 text-xs outline-none focus-visible:ring-2 focus-visible:ring-primary/40 sm:hidden">
            <SlidersHorizontal className="h-4 w-4" /><span>{t('catalog:filtersAndPricing')}</span><span className="ml-auto truncate text-muted-foreground">{groupInfo ? groupName(groupInfo) : group || t('catalog:baseShort')} · USD / {unit}</span><ChevronDown className={cn('h-4 w-4 shrink-0', filtersOpen && 'rotate-180')} />
          </button>
          <div id="catalog-filters" className={cn(!filtersOpen && 'hidden sm:block')}>
          <div className="my-4 flex flex-wrap items-center gap-2">
            <SlidersHorizontal className="mr-1 hidden h-4 w-4 text-muted-foreground sm:block" />
            <Select aria-label={t('catalog:billingMode')} value={search.mode ?? ''} onChange={(mode) => patch({ mode: mode || undefined })} placeholder={t('catalog:allBilling')} options={['ratio', 'per_call', 'tiered'].map((mode) => ({ value: mode, label: t(`analysis:${mode}`) }))} />
            {capabilities.length > 0 && <Select aria-label={t('catalog:capabilities')} value={search.capability ?? ''} onChange={(capability) => patch({ capability: capability || undefined })} placeholder={t('catalog:allCapabilities')} options={capabilities.map((cap) => ({ value: cap, label: t(`catalog:cap_${cap}`) }))} />}
            <label className="flex min-h-9 cursor-pointer items-center gap-2 rounded-lg px-2 text-xs text-muted-foreground"><input type="checkbox" className="h-4 w-4 accent-primary" checked={!!search.available} onChange={(e) => patch({ available: e.target.checked || undefined })} />{t('catalog:onlyAvailable')}</label>
            {active && <Button size="sm" variant="ghost" onClick={clear}><X className="h-3.5 w-3.5" />{t('catalog:clearFilters')}</Button>}
          </div>
          <div className="flex flex-wrap items-end justify-between gap-3 rounded-xl border border-border bg-card p-3 sm:p-4">
            <div className="flex min-w-0 max-w-full flex-col gap-1.5"><Label htmlFor="catalog-group">{t('pricing:viewAsGroup')}</Label>
              <Select id="catalog-group" className="w-64 max-w-full" value={group} onChange={(group) => patch({ group: group || undefined })} placeholder={t('pricing:baseGroup')}
                disabled={!pricing.isSuccess} options={groups.map((g) => ({ value: g.code, label: `${groupName(g)} ×${formatRatio(g.ratio)}` }))} /></div>
            <div className="flex flex-col gap-1.5"><Label>{t('catalog:currencyUnit')}</Label><Segmented ariaLabel={t('pricing:tokenUnit')} value={unit} onChange={(unit) => patch({ unit: unit === '1M' ? undefined : unit })} options={[{ value: '1M', label: '1M tokens' }, { value: '1K', label: '1K tokens' }]} /></div>
            <p className="w-full text-xs leading-5 text-muted-foreground">{t(signedIn ? 'catalog:myGroupsHint' : 'catalog:publicGroupsHint')} {t('catalog:priceNote')}</p>
          </div>
          </div>
          {pricing.isError ? <div className="mt-5"><ErrorState message={describeError(pricing.error)} onRetry={() => void pricing.refetch()} /></div> : (pricing.isPending || outOfRange) ? <LoadingState /> : <>
            <div className="my-5 flex flex-wrap items-center justify-between gap-3">
              <h2 ref={resultsHeading} tabIndex={-1} className="scroll-mt-24 text-sm font-semibold outline-none" aria-live="polite">{activeVendor ? activeVendor.name || t('catalog:otherVendor') : t('catalog:allModels')}<span className="ml-2 font-normal text-muted-foreground">{t('catalog:resultCount', { n: total })}</span></h2>
              <div className="flex flex-wrap items-center gap-2"><Select aria-label={t('catalog:sort')} value={search.sort ?? 'name'} onChange={(sort) => patch({ sort: sort as CatalogSearch['sort'] })}
                options={[{ value: 'name', label: t('catalog:sortName') }, { value: 'input', label: t('catalog:sortInput') }, { value: 'output', label: t('catalog:sortOutput') }, ...(catalog.stats.data?.has_context ? [{ value: 'context', label: t('catalog:sortContext') }] : [])]} />
                <Segmented ariaLabel={t('catalog:view')} value={search.view ?? 'cards'} onChange={(view) => patch({ view: view === 'cards' ? undefined : view })} options={[{ value: 'cards', label: t('catalog:cards'), icon: LayoutGrid }, { value: 'table', label: t('catalog:table'), icon: List }]} /></div>
            </div>
            {total === 0 ? <EmptyState title={t('common:noResults')} hint={totalModels ? t('catalog:emptyHint') : t('catalog:emptyCatalog')}
              action={active ? <Button variant="outline" size="sm" onClick={clear}>{t('catalog:clearFilters')}</Button> : undefined} />
              : search.view === 'table' ? <Table><THead><Tr><Th>{t('pricing:model')}</Th><Th>{t('catalog:specifications')}</Th><Th>{t('catalog:billingMode')}</Th><Th numeric>{t('pricing:promptPrice')} / {unit}</Th><Th numeric>{t('pricing:completionPrice')} / {unit}</Th><Th>{t('catalog:availability')}</Th><Th><span className="sr-only">{t('catalog:details')}</span></Th></Tr></THead><TBody>
                {shown.map((model) => <ModelTableRow key={model.model} model={model} groups={groups} group={group} factor={factor} unit={unit} onOpen={() => patch({ model: model.model, tab: undefined }, false)} onExamples={() => patch({ model: model.model, tab: 'code' }, false)} {...compareProps(model)} />)}
              </TBody></Table> : <div className="grid grid-cols-1 gap-4 md:grid-cols-2 xl:grid-cols-3">
                {shown.map((model) => <ModelCard key={model.model} model={model} groups={groups} group={group} factor={factor} unit={unit} onOpen={() => patch({ model: model.model, tab: undefined }, false)} onExamples={() => patch({ model: model.model, tab: 'code' }, false)} {...compareProps(model)} />)}
              </div>}
            {total > 0 && <div className="mt-6 flex flex-col gap-3 rounded-xl border border-border bg-card p-4">
              <div className="flex items-center justify-between gap-2"><Label htmlFor="catalog-page-size">{t('catalog:pageSize')}</Label><Select id="catalog-page-size" value={String(pageSize)} onChange={(size) => patch({ pageSize: size === '24' ? undefined : Number(size) as 12 | 48 })} options={[12, 24, 48].map((n) => ({ value: String(n), label: t('catalog:perPage', { n }) }))} /></div>
              <Pagination total={total} limit={pageSize} offset={offset} onOffset={changePage} />
            </div>}
            <p className="mt-6 text-xs leading-5 text-muted-foreground">{t('catalog:availabilityHint')}</p>
          </>}
          {lookupError && <div className="mt-4"><ErrorState message={describeError(lookupError.error)} onRetry={() => void Promise.all(catalog.details.filter((query) => query.isError).map((query) => query.refetch()))} /></div>}
          {search.model && !selected && pricing.isSuccess && detailQuery?.isSuccess && <div className="mt-4"><EmptyState title={t('catalog:modelMissing')} action={<Button variant="outline" onClick={() => patch({ model: undefined })}>{t('common:close')}</Button>} /></div>}
        </div>
      </div>
    </main>
    {selected && <ModelDetails key={selected.model} model={selected} groups={groups} group={group} factor={factor} unit={unit} tab={search.tab ?? 'details'} onTab={(tab) => patch({ tab: tab === 'code' ? 'code' : undefined })} onGroup={(group) => patch({ group: group || undefined })} onClose={() => patch({ model: undefined, tab: undefined })} />}
    {compared.length > 0 && <CompareTray models={compared} onRemove={toggleCompare} onClear={() => setCompare([])} onStart={() => patch({ comparing: true }, false)} />}
    {search.comparing && compared.length >= 2 && <CompareDrawer models={compared} group={group} factor={factor} unit={unit} onRemove={toggleCompare} onClose={() => patch({ comparing: undefined })} />}
    <footer className="border-t border-border"><div className="mx-auto flex max-w-[1480px] flex-wrap items-center justify-between gap-x-6 gap-y-2 px-4 py-6 text-xs text-muted-foreground sm:px-8"><span>{t('common:appName')}</span><span>{t('catalog:footer')}</span><PoweredBy /></div></footer>
  </div>
}
