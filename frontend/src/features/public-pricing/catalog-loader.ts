import { apiFetch, ApiError } from '@/lib/api'
import type { PricingGroup, PricingModel } from './types'

export interface Catalog { models: PricingModel[]; groups: PricingGroup[] }
export interface PageMeta { total: number; limit: number; offset: number; has_more: boolean; next_offset: number | null }
export interface CatalogPage extends Catalog, Partial<PageMeta> { groups_page?: PageMeta; pricing_epoch?: number }
const batchSize = 100 // Public catalog endpoints cap each independent page at 100.
const invalidPage = () => new ApiError(502, 'internal_error')

export function pagination(meta: Partial<PageMeta>, count: number, offset: number, total?: number): PageMeta | undefined {
  // Compatibility with older servers that returned a complete, unpaged catalog.
  if (meta.total === undefined) {
    if (total !== undefined || ['limit', 'offset', 'has_more', 'next_offset'].some((key) => key in meta)) throw invalidPage()
    return undefined
  }
  if (!Number.isSafeInteger(meta.total) || meta.total < 0
    || !Number.isSafeInteger(meta.limit) || meta.limit! < 1 || count > meta.limit!
    || meta.offset !== offset || (total !== undefined && meta.total !== total)
    || (offset + count > meta.total && !(count === 0 && offset >= meta.total))) throw invalidPage()
  const more = offset + count < meta.total
  if (meta.has_more !== more || (more && count === 0)
    || meta.next_offset !== (more ? offset + count : null)) throw invalidPage()
  return meta as PageMeta
}

function path(modelOffset: number, modelLimit: number, groupOffset = 0): string {
  return `/api/pricing?limit=${modelLimit}&offset=${modelOffset}&group_limit=${batchSize}&group_offset=${groupOffset}`
}

// Complete directory for API-key model allowlist selectors. The marketplace
// uses loadModelPage instead; it never drains the model pagination chain.
// Group pages affect each model's availability/endpoints, so merge those too.
export async function loadCatalog(key: string): Promise<Catalog> {
  const models: PricingModel[] = []
  const names = new Set<string>()
  const groups = new Map<string, PricingGroup>()
  let modelOffset = 0
  let modelTotal: number | undefined
  let groupTotal: number | undefined
  let epoch: number | undefined
  let firstPage = true
  const checkEpoch = (page: CatalogPage) => {
    if (page.pricing_epoch !== undefined && (!Number.isSafeInteger(page.pricing_epoch) || page.pricing_epoch < 0)) throw invalidPage()
    if (!firstPage && page.pricing_epoch !== epoch) throw invalidPage()
    epoch = page.pricing_epoch
    firstPage = false
  }
  let requestPath = '/api/pricing'
  for (;;) {
    const response = await apiFetch<CatalogPage>(requestPath, { key })
    checkEpoch(response)
    const modelPage = pagination(response, response.models.length, modelOffset, modelTotal)
    modelTotal = modelPage?.total
    const batch = response.models.map((model) => ({ ...model, groups: [...model.groups],
      chat_endpoints_by_group: model.chat_endpoints_by_group && { ...model.chat_endpoints_by_group },
    }))
    let groupResponse = response
    let groupOffset = 0
    const batchGroups = new Set<string>()
    for (;;) {
      const groupPage = pagination(groupResponse.groups_page ?? {}, groupResponse.groups.length, groupOffset, groupTotal)
      groupTotal = groupPage?.total
      for (const group of groupResponse.groups) {
        if (batchGroups.has(group.code)) throw invalidPage()
        batchGroups.add(group.code)
        groups.set(group.code, group)
      }
      if (!groupPage?.has_more) break
      groupOffset = groupPage.next_offset!
      groupResponse = await apiFetch<CatalogPage>(path(modelOffset, modelPage?.limit ?? Math.max(1, batch.length), groupOffset), { key })
      checkEpoch(groupResponse)
      const repeatedPage = pagination(groupResponse, groupResponse.models.length, modelOffset, modelTotal)
      if (!!repeatedPage !== !!modelPage || groupResponse.models.length !== batch.length) throw invalidPage()
      for (let i = 0; i < batch.length; i++) {
        const extra = groupResponse.models[i], model = batch[i]
        if (extra.model !== model.model) throw invalidPage()
        model.groups = [...new Set([...model.groups, ...extra.groups])]
        if (extra.chat_endpoints_by_group) {
          model.chat_endpoints_by_group = { ...model.chat_endpoints_by_group, ...extra.chat_endpoints_by_group }
        }
      }
    }
    if (groupTotal !== undefined && (batchGroups.size !== groupTotal || groups.size !== groupTotal)) throw invalidPage()
    for (const model of batch) {
      // Repeated/empty pages or changing totals must not silently hide models
      // or cause an endless request loop. Retry starts with a fresh directory.
      if (names.has(model.model)) throw invalidPage()
      names.add(model.model)
      models.push(model)
    }
    if (!modelPage?.has_more) return { models, groups: [...groups.values()] }
    modelOffset = modelPage.next_offset!
    requestPath = path(modelOffset, batchSize)
  }
}
