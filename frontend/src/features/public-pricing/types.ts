export interface PricingModel {
  model: string
  display_name: string | null
  vendor: string | null
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
