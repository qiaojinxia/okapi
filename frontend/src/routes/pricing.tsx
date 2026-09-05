import { createFileRoute } from '@tanstack/react-router'
import { PublicPricingPage } from '@/features/public-pricing/PublicPricingPage'
import type { CatalogSearch } from '@/features/public-pricing/types'

const text = (value: unknown) => typeof value === 'string' && value.length > 0 ? value.slice(0, 256) : undefined

export const Route = createFileRoute('/pricing')({
  validateSearch: (search: Record<string, unknown>): CatalogSearch => ({
    q: text(search.q), vendor: text(search.vendor), group: text(search.group), model: text(search.model),
    mode: ['ratio', 'per_call', 'tiered'].includes(String(search.mode)) ? String(search.mode) : undefined,
    capability: text(search.capability), available: search.available === true || search.available === 'true' || undefined,
    unit: search.unit === '1K' ? '1K' : undefined,
    view: search.view === 'table' ? 'table' : undefined,
    sort: search.sort === 'input' || search.sort === 'output' || search.sort === 'context' ? search.sort : undefined,
    tab: search.tab === 'code' ? 'code' : undefined,
    page: Number.isSafeInteger(Number(search.page)) && Number(search.page) > 1 ? Math.min(Number(search.page), 1_000_000) : undefined,
    pageSize: Number(search.pageSize) === 12 ? 12 : Number(search.pageSize) === 48 ? 48 : undefined,
  }),
  component: PublicPricingPage,
})
