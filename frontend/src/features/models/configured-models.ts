import { queryOptions } from '@tanstack/react-query'
import { loadAll } from '@/lib/load-all'
import { qk } from '@/lib/query-keys'

export interface ConfiguredModel {
  model_name: string
  display_name?: string | null
  vendor: string | null
  pricing_mode: string | null
}

// The list endpoint is paginated even without limit. Collect every page before
// exposing options, so a partial catalog never looks like a model is missing.
async function loadConfiguredModels() {
  const data = await loadAll<ConfiguredModel>('/admin/models', (model) => model.model_name)
  return { data, total: data.length }
}

export const configuredModelsOptions = () => queryOptions({
  queryKey: qk.adminModelOptions,
  queryFn: loadConfiguredModels,
  staleTime: 60_000,
})
