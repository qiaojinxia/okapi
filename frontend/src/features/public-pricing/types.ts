export interface PricingModel {
  base_price_per_1m_micro?: number
  model: string
  display_name: string | null
  vendor: string | null
  capabilities?: Record<string, boolean>
  context_window?: number | null
  max_output?: number | null
  catalog_config?: import('@/features/models/types').ModelCatalogConfig
  modality_ratios?: Record<string, string | number> | null
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
  /// 按分组池链计算的聊天接口配置；不代表上游实时健康。
  chat_endpoints_by_group?: Record<string, string[]>
}

export interface PricingGroup {
  code: string
  name: string | null
  ratio: string | null
  self_select?: boolean
  is_default?: boolean
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
  /// 对比中的模型 ID，逗号分隔，最多 4 个；进 URL，可分享、刷新后恢复。
  compare?: string
  /// 对比抽屉是否打开（至少选了 2 个才会真正打开）。
  comparing?: boolean
  tab?: 'code'
  page?: number
  pageSize?: 12 | 24 | 48
}
