export interface ModelListRow {
  model_name: string
  display_name?: string | null
  capabilities?: Record<string, boolean>
  context_window?: number | null
  max_output?: number | null
  catalog_config?: ModelCatalogConfig
  vendor: string | null
  status: number
  pricing_mode: string | null
  model_ratio: string | null
  completion_ratio: string | null
  cache_ratio: string | null
  cache_write_ratio: string | null
  /// 阶梯计价表 "0:2.5,128000:5"；非空 = tiered 模式。
  tier_expr: string | null
  audio_ratio: string | null
  audio_completion_ratio: string | null
  image_ratio: string | null
  per_call_price_micro: number | null
  tier_ratios?: Record<string, string> | null
  modality_ratios?: Record<string, string> | null
  /// 模型级降级链：零可用候选时按序改投（单跳），计费按实际服务模型。
  fallback_models: string[]
}

export interface ModelCatalogConfig {
  kind?: string | null
  description?: string | null
  input_modalities?: string[]
  output_modalities?: string[]
}

export const MODEL_KINDS = ['chat', 'completion', 'embedding', 'rerank', 'image_generation', 'speech_to_text', 'text_to_speech', 'realtime', 'video_generation', 'moderation', 'search'] as const
export const CAPABILITY_KEYS = ['vision', 'tools', 'parallel_tools', 'json', 'structured_output', 'reasoning', 'audio', 'video', 'embedding', 'realtime', 'streaming', 'prompt_cache', 'system_prompt', 'temperature', 'web_search', 'computer_use'] as const
export const INDEPENDENT_AXES = ['cache_write_5m', 'cache_write_1h', 'image_cache_read', 'audio_cache_read', 'image_cache_write', 'audio_cache_write', 'image_output'] as const



/// 倍率轴。分两组呈现：文本轴人人都要配，多模态轴只有音频/图片模型才需要——
/// 把八个输入框平铺会让人以为每个都必填。
export const TEXT_AXES = ['model_ratio', 'completion_ratio', 'cache_ratio', 'cache_write_ratio'] as const


export const MODAL_AXES = ['audio_ratio', 'audio_completion_ratio', 'image_ratio'] as const



/// 轴 → 文案键的显式映射。不用 `t(`admin:${key}`)` 动态拼：拼错了运行时不报错，
/// 只是把原始键渲染到界面，而文案闸门也查不出来。
export const AXIS_LABEL = {
  model_ratio: 'admin:modelRatio',
  completion_ratio: 'admin:completionRatio',
  cache_ratio: 'admin:cacheRatio',
  cache_write_ratio: 'admin:cacheWriteRatio',
  audio_ratio: 'admin:audioRatio',
  audio_completion_ratio: 'admin:audioCompletionRatio',
  image_ratio: 'admin:imageRatio',
} as const
