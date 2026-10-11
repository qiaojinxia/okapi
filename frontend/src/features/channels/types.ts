import type { PoolMember } from '@/features/pools/types'
import type { EgressBinding } from '@/features/proxies/types'

/// 渠道协议：决定请求如何被转换后送往上游（见 §4.4 四象限）。
/// `openai` = 官方 OpenAI（/v1/responses 缺省直转）；`openai_compat` = 一切 OpenAI 兼容上游
/// （只保证 chat，Responses 缺省降级）；`azure` = Azure OpenAI（部署 URL + api-key 头 +
/// api-version，模型映射的值即部署名）；`bedrock` = Amazon Bedrock（InvokeModel，Anthropic 方言，
/// SigV4 或 Bedrock API key）；`vertex` = Google Vertex AI（服务账号 OAuth，Claude / Gemini）
/// ——与后端 docs/database.md channels.provider 枚举一致。
export const PROVIDERS = [
  'openai',
  'openai_compat',
  'azure',
  'anthropic',
  'gemini',
  'bedrock',
  'vertex',
  'anthropic_max',
  'codex',
  'custom_pass',
] as const
export type Provider = (typeof PROVIDERS)[number]

/// 只服务 chat 族入口、且 api_base 必填的云厂商托管上游（IMPLEMENTATION §11.35）。
export function isCloudManaged(provider: string): boolean {
  return provider === 'bedrock' || provider === 'vertex'
}

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
      return 'https://api.anthropic.com/v1'
    case 'gemini':
      return 'https://generativelanguage.googleapis.com/v1beta'
    case 'bedrock':
      return 'https://bedrock-runtime.us-east-1.amazonaws.com'
    case 'vertex':
      return 'https://us-central1-aiplatform.googleapis.com/v1/projects/{project}/locations/us-central1'
    case 'anthropic_max':
      return 'https://api.anthropic.com/v1'
    case 'codex':
      return 'https://chatgpt.com/backend-api/codex'
    case 'custom_pass':
      return 'https://upstream.example.com'
    default:
      return 'https://api.openai.com/v1'
  }
}

/// UI defaults mirror the provider registry; unknown/cloud endpoints must stay visible.
export function defaultApiBase(provider: string): string | undefined {
  return ['openai', 'openai_compat', 'anthropic', 'gemini', 'anthropic_max', 'codex'].includes(provider)
    ? apiBasePlaceholder(provider) : undefined
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
  /// 0 = 静态 key，1 = OAuth 订阅凭证（§11.38）。
  credential_kind: number
  /// 固定分配组分给这把 key（账号）的代理；null = 未分配 / 不是固定分配。
  egress_proxy_id?: number | null
  /// OAuth 凭证的 access token 到期（unix 秒）；静态 key 无此字段。
  credential_expires_at?: number
  /// False means access-token-only: no automatic or manual refresh authority.
  oauth_refreshable?: boolean
  /// 订阅账号邮箱（换码时记录），仅展示
  account_label?: string
  /// 订阅档位（额度探测时顺带查 profile，每天一次）：pro / max_5x / max_20x / max / team / enterprise
  account_plan?: string
  oauth_refresh?: {
    last_attempt_at: number | null
    last_success_at: number | null
    consecutive_failures: number
    next_retry_at: number | null
    error_code: string | null
  }
}



/// 渠道行为开关。后端只认这几个键，故前端用具名字段而非任意 JSON——
/// 用户不该去猜有哪些键可填、值是什么类型。
export interface ChannelSettings {
  account_control?: AccountControl
  thinking_to_content: boolean
  bill_by_response_model: boolean
  strip_request_fields: string[]
  /// `/v1/responses` 是否同方言直转到该渠道；undefined = 跟随协议缺省
  /// （openai 直转 / openai_compat 降级）。只对说 OpenAI 方言的渠道有意义。
  responses_native?: boolean
  /// Azure 数据面 api-version（`YYYY-MM-DD[-preview]`）；undefined = 后端缺省。只对 azure 有意义。
  api_version?: string
  /// Bedrock SigV4 区域覆写；undefined = 从 api_base 主机名解析。只对 bedrock 有意义。
  aws_region?: string
  /// 额外请求头（对象）。鉴权 / Host / 逐跳头后端会拒。
  extra_headers?: Record<string, string>
  /// 强制写入请求顶层的字段。model / messages / stream / provider 后端会拒。
  inject_request_fields?: Record<string, unknown>
  extensions?: {
    client_profile?: { name: 'native' } | {
      name: 'claude-code'
      mode: 'auto' | 'passthrough' | 'mimic'
      /** Server-normalized; omitted means the latest client the server implements. */
      revision?: string
      entrypoint?: 'cli' | 'sdk-cli'
      request_class?: 'main' | 'auxiliary'
    }
  }
}

export interface AccountControl {
  /** Historical local caps are read for the retirement notice and removed on save. */
  usage?: { period?: 'hour' | 'day' | 'week' | 'month'; requests?: number | null; tokens?: number | null; cost_micro?: number | null }
  quota_aware: boolean
  quota_threshold_pct: number
  /** Upstream window seconds mapped to independently configured percentages. */
  quota_limits?: Record<string, number>
  local_tokens?: { cap: number; period?: 'total' | 'day' | 'week' } | null
  rate_limit_cooldown_secs: number
  failure_threshold: number
  failure_cooldown_secs: number
  refresh_mode: 'managed' | 'external'
  refresh_margin_secs: number
}

export function channelSettingsForSave(settings: ChannelSettings): ChannelSettings {
  if (!settings.account_control) return settings
  const { usage: _retired, ...account_control } = settings.account_control
  return { ...settings, account_control }
}



/// 最近一次测活（Redis 30 天 TTL；没测过/已过期为 null）。
export interface ChannelProbe {
  ok: boolean
  latency_ms: number
  http_status?: number
  error_code?: string
  at: string
}

/// 最近一次上游余额查询（Redis 30 天 TTL；没查过 / 已过期为 null，IMPLEMENTATION §11.33）。
/// 金额是 `currency` 的 micro 整数，不是站内 USD 账。
export interface ChannelBalance {
  probe: string
  currency: string
  balance_micro: number
  total_micro: number | null
  used_micro: number | null
  at: string
}

/// 哪些协议有可查的余额接口（与后端 `Probe::for_channel` 一致）：
/// anthropic / gemini / azure / custom_pass 没有公开余额接口，按钮不显示。
export function balanceSupported(provider: string): boolean {
  return provider === 'openai' || provider === 'openai_compat'
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
  /// 出口绑定（§11.41）：继承全局默认 / 直连 / 单个代理 / 代理组。
  egress?: EgressBinding
  last_test: ChannelProbe | null
  last_balance: ChannelBalance | null
}



export function readSettings(raw: Partial<ChannelSettings> | null): ChannelSettings {
  // settings.proxy_url 已退役（§11.41 出口绑定）：残留值不能被抽屉整体回写（后端会 400）
  const { proxy_url: _retired, ...opaque } = (raw ?? {}) as Partial<ChannelSettings> & { proxy_url?: unknown }
  return {
    // Preserve opaque provider/extension settings when editing unrelated form fields.
    ...opaque,
    thinking_to_content: raw?.thinking_to_content ?? false,
    bill_by_response_model: raw?.bill_by_response_model ?? false,
    strip_request_fields: raw?.strip_request_fields ?? [],
    // 不在这里补缺省：未配置就保持 undefined，保存回去仍是"跟随协议缺省"，
    // 站长改协议时不会被一个早先写死的布尔值绊住
    ...(typeof raw?.responses_native === 'boolean' ? { responses_native: raw.responses_native } : {}),
    ...(typeof raw?.api_version === 'string' && raw.api_version !== ''
      ? { api_version: raw.api_version }
      : {}),
    ...(typeof raw?.aws_region === 'string' && raw.aws_region !== ''
      ? { aws_region: raw.aws_region }
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
    case 'bedrock':
      return 'https://console.aws.amazon.com/bedrock/'
    case 'vertex':
      return 'https://console.cloud.google.com/vertex-ai'
    case 'anthropic_max':
      return 'https://claude.ai/settings/usage'
    case 'codex':
      return 'https://chatgpt.com/'
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
