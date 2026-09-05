export interface PricingModel {
  model: string
  display_name: string | null
  vendor: string | null
  capabilities?: Record<string, boolean>
  context_window?: number | null
  max_output?: number | null
  mode: string
  model_ratio: string | null
  completion_ratio: string | null
  cache_ratio: string | null
  cache_write_ratio: string | null
  audio_ratio: string | null
  audio_completion_ratio: string | null
  image_ratio: string | null
  per_call_price_micro: number | null
  /// 可用分组（按池可见性折算的静态视图）；空 = 当前没有渠道服务该模型。
  groups: string[]
}

export interface PricingGroup {
  code: string
  name: string | null
  ratio: string | null
}

export type TokenUnit = '1K' | '1M'
export interface CatalogSearch {
  q?: string
  vendor?: string
  group?: string
  mode?: string
  capability?: string
  available?: boolean
  unit?: TokenUnit
  view?: 'cards' | 'table'
  sort?: 'name' | 'input' | 'output' | 'context'
  model?: string
  tab?: 'code'
  page?: number
  pageSize?: 12 | 24 | 48
}
