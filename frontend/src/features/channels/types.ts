import type { PoolMember } from '@/features/pools/types'

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



/// 最近一次测活（Redis 30 天 TTL；没测过/已过期为 null）。
export interface ChannelProbe {
  ok: boolean
  latency_ms: number
  http_status?: number
  error_code?: string
  at: string
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
  /// 所属池代码；空数组 = 孤儿（不在任何池，对谁都不可达）。
  pools: string[]
  /// 池成员关系明细（含成员级 priority / weight 覆盖）。
  pool_members: PoolMember[]
  last_test: ChannelProbe | null
}



export function readSettings(raw: Partial<ChannelSettings> | null): ChannelSettings {
  return {
    thinking_to_content: raw?.thinking_to_content ?? false,
    bill_by_response_model: raw?.bill_by_response_model ?? false,
    strip_request_fields: raw?.strip_request_fields ?? [],
  }
}

/// 供应商控制台地址（new-api #7146"渠道里加供应商网站跳转"）：查余额 / 看状态页时
/// 直达。已知供应商给控制台用量页；OpenAI 兼容与自定义透传取 api_base 的站点根——
/// 多数兼容站的控制台就在同一域名下；解析不出合法 URL 时不显示链接。
export function providerConsoleUrl(provider: string, apiBase: string | null): string | null {
  switch (provider) {
    case 'openai':
      return 'https://platform.openai.com/usage'
    case 'anthropic':
      return 'https://console.anthropic.com/settings/usage'
    case 'gemini':
      return 'https://aistudio.google.com/'
    default: {
      if (apiBase === null) return null
      try {
        const u = new URL(apiBase)
        return u.protocol === 'https:' || u.protocol === 'http:' ? u.origin : null
      } catch {
        return null
      }
    }
  }
}
