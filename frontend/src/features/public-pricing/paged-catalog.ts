import { apiFetch, ApiError } from '@/lib/api'
import { modelVendor, vendorFilter } from './catalog-data'
import { pagination } from './catalog-loader'
import type { CatalogPage, PageMeta } from './catalog-loader'
import type { CatalogSearch } from './types'

export interface CatalogStatistics {
  total: number
  capabilities: string[]
  has_context: boolean
  vendors: Array<{ vendor: string | null; count: number }>
  vendors_page: PageMeta
  pricing_epoch: number
}
export type ModelPage = CatalogPage & PageMeta
const invalid = () => new ApiError(502, 'internal_error')

export function catalogParams(search: CatalogSearch): string {
  const params = new URLSearchParams({ limit: String(search.pageSize ?? 24), offset: String(((search.page ?? 1) - 1) * (search.pageSize ?? 24)), sort: search.sort ?? 'name' })
  if (search.q?.trim()) params.set('q', search.q.trim())
  if (search.vendor) for (const [key, value] of Object.entries(vendorFilter(search.vendor))) params.set(key, value)
  if (search.mode) params.set('mode', search.mode)
  if (search.capability) params.set('capability', search.capability)
  if (search.available) params.set('available', 'true')
  if (search.group && (search.available || search.sort === 'input' || search.sort === 'output')) params.set('availability_group', search.group)
  return params.toString()
}

// Expand group metadata only for this model page. Never follow the models' next_offset.
export async function loadModelPage(params: string, key: string, signal?: AbortSignal): Promise<ModelPage> {
  const query = new URLSearchParams(params)
  query.set('group_limit', '100')
  const offset = Number(query.get('offset') ?? 0)
  const first = await apiFetch<ModelPage>(`/api/pricing?${query}`, { key, signal })
  if (!pagination(first, first.models.length, offset) || first.limit !== Number(query.get('limit'))) throw invalid()
  const models = first.models.map((model) => ({ ...model, groups: [...model.groups], chat_endpoints_by_group: model.chat_endpoints_by_group && { ...model.chat_endpoints_by_group } }))
  if (new Set(models.map((m) => m.model)).size !== models.length) throw invalid()
  const groups = [...first.groups]
  let response = first, groupOffset = 0
  for (;;) {
    const meta = pagination(response.groups_page ?? {}, response.groups.length, groupOffset, first.groups_page?.total)
    if (!meta) throw invalid()
    if (!meta.has_more) break
    groupOffset = meta.next_offset!
    query.set('group_offset', String(groupOffset))
    response = await apiFetch<ModelPage>(`/api/pricing?${query}`, { key, signal })
    if (response.pricing_epoch !== first.pricing_epoch || !pagination(response, response.models.length, offset, first.total)
      || response.models.length !== models.length) throw invalid()
    for (let i = 0; i < models.length; i++) {
      const model = models[i], extra = response.models[i]
      if (model.model !== extra.model) throw invalid()
      model.groups = [...new Set([...model.groups, ...extra.groups])]
      model.chat_endpoints_by_group = { ...model.chat_endpoints_by_group, ...extra.chat_endpoints_by_group }
    }
    groups.push(...response.groups)
  }
  if (new Set(groups.map((g) => g.code)).size !== groups.length) throw invalid()
  return { ...first, models, groups }
}

// Facet pages contain only counts, never model objects. Counts remain independent of the card page.
export async function loadCatalogStatistics(key: string, signal?: AbortSignal): Promise<CatalogStatistics> {
  const first = await apiFetch<CatalogStatistics>('/api/pricing/stats?vendor_limit=100', { key, signal })
  if (!Number.isSafeInteger(first.total) || first.total < 0 || !Array.isArray(first.capabilities) || typeof first.has_context !== 'boolean') throw invalid()
  const vendors = [...first.vendors]
  let response = first, offset = 0
  for (;;) {
    const meta = pagination(response.vendors_page, response.vendors.length, offset, first.vendors_page.total)
    if (!meta) throw invalid()
    if (!meta.has_more) break
    offset = meta.next_offset!
    response = await apiFetch<CatalogStatistics>(`/api/pricing/stats?vendor_limit=100&vendor_offset=${offset}`, { key, signal })
    if (response.pricing_epoch !== first.pricing_epoch || response.total !== first.total) throw invalid()
    vendors.push(...response.vendors)
  }
  if (new Set(vendors.map((v) => v.vendor)).size !== vendors.length
    || vendors.some((v) => !Number.isSafeInteger(v.count) || v.count <= 0)
    || vendors.reduce((sum, v) => sum + v.count, 0) !== first.total) throw invalid()
  return { ...first, vendors }
}

export function catalogVendors(statistics?: CatalogStatistics) {
  const map = new Map<string, { vendor: ReturnType<typeof modelVendor>; count: number }>()
  for (const item of statistics?.vendors ?? []) {
    const vendor = modelVendor(item), existing = map.get(vendor.id)
    if (existing) existing.count += item.count
    else map.set(vendor.id, { vendor, count: item.count })
  }
  return [...map.values()].sort((a, b) => a.vendor.id === 'other' ? 1 : b.vendor.id === 'other' ? -1 : b.count - a.count || a.vendor.name.localeCompare(b.vendor.name))
}

export async function loadCatalogModel(id: string, key: string, signal?: AbortSignal): Promise<ModelPage> {
  const result = await loadModelPage(new URLSearchParams({ model: id, limit: '1', offset: '0' }).toString(), key, signal)
  if (result.total > 1 || result.models.some((m) => m.model !== id)) throw invalid()
  return result
}
