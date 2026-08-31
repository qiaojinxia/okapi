/// 渠道协议：决定请求如何被转换后送往上游（见 §4.4 四象限）。
export const PROVIDERS = ['openai', 'anthropic', 'gemini', 'custom_pass'] as const



export interface ChannelKeyRow {
  id: number
  status: number
  failed_count: number
  cooldown_until: string | null
  last_error: string | null
  weight: number
  max_concurrency: number | null
}



/// 渠道行为开关。后端只认这三个键，故前端用具名字段而非任意 JSON——
/// 用户不该去猜有哪些键可填、值是什么类型。
export interface ChannelSettings {
  thinking_to_content: boolean
  bill_by_response_model: boolean
  strip_request_fields: string[]
}



export interface ChannelRow {
  id: number
  name: string
  provider: string
  api_base: string | null
  status: number
  priority: number
  models: string[]
  keys: ChannelKeyRow[]
  settings: Partial<ChannelSettings> | null
  pools: string[]
}



export function readSettings(raw: Partial<ChannelSettings> | null): ChannelSettings {
  return {
    thinking_to_content: raw?.thinking_to_content ?? false,
    bill_by_response_model: raw?.bill_by_response_model ?? false,
    strip_request_fields: raw?.strip_request_fields ?? [],
  }
}
