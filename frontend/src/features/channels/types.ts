import type { PoolMember } from '@/features/pools/types'

/// 渠道协议：决定请求如何被转换后送往上游（见 §4.4 四象限）。
/// `openai` = 官方 OpenAI（/v1/responses 缺省直转）；`openai_compat` = 一切 OpenAI 兼容上游
/// （只保证 chat，Responses 缺省降级）；`azure` = Azure OpenAI（部署 URL + api-key 头 +
/// api-version，模型映射的值即部署名）——与后端 docs/database.md channels.provider 枚举一致。
export const PROVIDERS = [
  'openai',
  'openai_compat',
  'azure',
  'anthropic',
  'gemini',
  'custom_pass',
] as const
export type Provider = (typeof PROVIDERS)[number]

/// 说 OpenAI 方言、因而有 Responses 直转/降级之选的协议。
/// azure 虽同方言，但其 Responses 走另一套 `/openai/v1` 路径，本期不给直转选项。
export function speaksOpenAi(provider: string): boolean {
  return provider === 'openai' || provider === 'openai_compat'
}

/// 各协议 api_base 的占位示例（azure 是资源端点，没有可猜的缺省，必填）。
export function apiBasePlaceholder(provider: string): string {
  switch (provider) {
    case 'azure':
      return 'https://{resource}.openai.azure.com'
    case 'anthropic':
      return 'https://api.anthropic.com'
    case 'gemini':
      return 'https://generativelanguage.googleapis.com/v1beta'
    default:
      return 'https://api.openai.com/v1'
  }
}

/// `responses_native` 未显式配置时的生效缺省（与后端 `responses_native_for` 一致）。
export function defaultResponsesNative(provider: string): boolean {
  return provider === 'openai'
}

/// `/admin/channels` 的 search params：分页 + 关键词 + 协议过滤。
export interface ChannelsSearch {
  page?: number
  limit?: number
  /// 关键词（名称 / 地址）。
  q?: string
  provider?: Provider
}


export interface ChannelKeyRow {
  id: number
  status: number
  failed_count: number
  cooldown_until: string | null
  last_error: string | null
  weight: number
  max_concurrency: number | null
}



/// 渠道行为开关。后端只认这几个键，故前端用具名字段而非任意 JSON——
/// 用户不该去猜有哪些键可填、值是什么类型。
export interface ChannelSettings {
  thinking_to_content: boolean
  bill_by_response_model: boolean
  strip_request_fields: string[]
  /// `/v1/responses` 是否同方言直转到该渠道；undefined = 跟随协议缺省
  /// （openai 直转 / openai_compat 降级）。只对说 OpenAI 方言的渠道有意义。
  responses_native?: boolean
  /// Azure 数据面 api-version（`YYYY-MM-DD[-preview]`）；undefined = 后端缺省。只对 azure 有意义。
  api_version?: string
  /// 出站代理（http / https / socks5 / socks5h）。undefined = 直连。
  proxy_url?: string
  /// 额外请求头（对象）。鉴权 / Host / 逐跳头后端会拒。
  extra_headers?: Record<string, string>
  /// 强制写入请求顶层的字段。model / messages / stream / provider 后端会拒。
  inject_request_fields?: Record<string, unknown>
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
  /// 相对成本系数（千分比；1000 = 官方标价）。毛利核算与调度加权共用。
  cost_milli: number
  /// 上游数据留存声明：none / transient / trains；null = 未声明。
  data_retention: string | null
  last_test: ChannelProbe | null
}



export function readSettings(raw: Partial<ChannelSettings> | null): ChannelSettings {
  return {
    thinking_to_content: raw?.thinking_to_content ?? false,
    bill_by_response_model: raw?.bill_by_response_model ?? false,
    strip_request_fields: raw?.strip_request_fields ?? [],
    // 不在这里补缺省：未配置就保持 undefined，保存回去仍是"跟随协议缺省"，
    // 站长改协议时不会被一个早先写死的布尔值绊住
    ...(typeof raw?.responses_native === 'boolean' ? { responses_native: raw.responses_native } : {}),
    ...(typeof raw?.api_version === 'string' && raw.api_version !== ''
      ? { api_version: raw.api_version }
      : {}),
    ...(typeof raw?.proxy_url === 'string' && raw.proxy_url.trim() !== ''
      ? { proxy_url: raw.proxy_url }
      : {}),
    ...(raw?.extra_headers && typeof raw.extra_headers === 'object' && !Array.isArray(raw.extra_headers)
      ? { extra_headers: raw.extra_headers }
      : {}),
    ...(raw?.inject_request_fields &&
    typeof raw.inject_request_fields === 'object' &&
    !Array.isArray(raw.inject_request_fields)
      ? { inject_request_fields: raw.inject_request_fields }
      : {}),
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
    case 'azure':
      // 资源端点是数据面地址，控制台在 Azure Portal / AI Foundry；部署管理走后者
      return 'https://ai.azure.com/'
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

/// 相对成本：千分比整数 ↔ 表单里的倍数字符串（"0.5" ↔ 500）。
/// 计费链路不碰浮点；这里只是把整数换成人看的写法，解析时四舍五入回整数。
export function costMilliToRatio(milli: number): string {
  return (milli / 1000).toString()
}

export function ratioToCostMilli(text: string): number | null {
  const v = Number(text.trim())
  if (text.trim() === '' || !Number.isFinite(v) || v < 0 || v > 100) return null
  return Math.round(v * 1000)
}
