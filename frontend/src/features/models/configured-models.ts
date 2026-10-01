import { queryOptions } from '@tanstack/react-query'
import { apiFetch, ApiError } from '@/lib/api'
import { qk } from '@/lib/query-keys'

export interface ConfiguredModel {
  model_name: string
  display_name?: string | null
  vendor: string | null
  pricing_mode: string | null
}

interface ModelPage {
  data: ConfiguredModel[]
  total?: number
}

// The list endpoint is paginated even without limit. Collect every page before
// exposing options, so a partial catalog never looks like a model is missing.
async function loadConfiguredModels() {
  const data: ConfiguredModel[] = []
  const names = new Set<string>()
  let path = '/admin/models'
  let total: number | undefined
  for (;;) {
    const page = await apiFetch<ModelPage>(path)
    total = page.total ?? total
    if (total !== undefined && (!Number.isSafeInteger(total) || total < 0)) {
      throw new ApiError(502, 'internal_error')
    }
    for (const model of page.data) {
      // A server ignoring offset must not loop or silently return duplicates.
      if (names.has(model.model_name)) throw new ApiError(502, 'internal_error')
      names.add(model.model_name)
      data.push(model)
    }
    // Older backends returned all models without a total field.
    if (total === undefined || data.length >= total) return { data, total: data.length }
    if (page.data.length === 0) throw new ApiError(502, 'internal_error')
    path = `/admin/models?limit=200&offset=${data.length}`
  }
}

export const configuredModelsOptions = () => queryOptions({
  queryKey: qk.adminModelOptions,
  queryFn: loadConfiguredModels,
  staleTime: 60_000,
})
