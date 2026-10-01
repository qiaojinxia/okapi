import type { PricingModel, TokenUnit } from './types'

export interface Vendor { id: string; name: string; icon?: string }

// 厂商来自服务端模型目录；别名只做展示归一，不根据 OpenAI 兼容协议推断厂商。
const brands: Array<[string, string, string, string[]]> = [
  ['openai', 'OpenAI', 'openai', ['open ai']],
  ['anthropic', 'Anthropic', 'anthropic', ['claude']],
  ['google', 'Google', 'google-color', ['gemini', 'google ai', 'google deepmind']],
  ['deepseek', 'DeepSeek', 'deepseek-color', ['deep seek']],
  ['alibaba', 'Alibaba · Qwen', 'qwen-color', ['qwen', 'aliyun', 'alibaba cloud', 'dashscope']],
  ['moonshot', 'Moonshot · Kimi', 'moonshot', ['moonshot ai', 'kimi']],
  ['zhipu', 'Zhipu · GLM', 'zai', ['zai', 'z.ai', 'zhipu ai', 'bigmodel']],
  ['bytedance', 'ByteDance', 'bytedance-color', ['doubao', 'volcengine']],
  ['minimax', 'MiniMax', 'minimax-color', []],
  ['xai', 'xAI', 'xai', ['x.ai', 'grok']],
  ['meta', 'Meta', 'meta-color', ['meta ai', 'llama']],
  ['mistral', 'Mistral AI', 'mistral-color', ['mistral ai']],
  ['baidu', 'Baidu', 'baidu-color', []],
  ['tencent', 'Tencent', 'tencent-color', ['hunyuan']],
  ['iflytek', 'iFlytek', 'spark-color', ['spark']],
  ['stepfun', 'StepFun', 'stepfun-color', []],
  ['baichuan', 'Baichuan', 'baichuan-color', []],
  ['01ai', '01.AI', 'yi-color', ['01.ai', 'yi']],
  ['kuaishou', 'Kuaishou', 'kling-color', ['kling']],
  ['ai360', '360 AI', 'ai360-color', ['360']],
  ['cohere', 'Cohere', 'cohere-color', []],
  ['jina', 'Jina AI', 'jina', ['jina ai']],
  ['amazon', 'Amazon', 'aws-color', ['aws']],
  ['stability', 'Stability AI', 'stability-color', ['stability ai']],
  ['ai21', 'AI21', 'ai21', ['ai21 labs']],
  ['nvidia', 'NVIDIA', 'nvidia-color', []],
  ['perplexity', 'Perplexity', 'perplexity-color', []],
  ['cloudflare', 'Cloudflare', 'cloudflare-color', []],
  ['vidu', 'Vidu', 'vidu-color', []],
  ['luma', 'Luma', 'luma', ['luma ai']],
  ['recraft', 'Recraft', 'recraft', []],
  ['blackforestlabs', 'Black Forest Labs', 'flux', ['black forest labs', 'bfl', 'flux']],
]
const normalize = (s: string) => s.trim().toLowerCase().replace(/[\s._-]+/g, '')
const registry = new Map(brands.flatMap(([id, name, icon, aliases]) =>
  [id, name, ...aliases].map((alias) => [normalize(alias), { id, name, icon }] as const)))

// 中文名称同样归一；显示名称仍取品牌注册表。
const localizedAliases: Array<[string, string]> = [
  ["\u8c37\u6b4c", "google"],
  ["\u6df1\u5ea6\u6c42\u7d22", "deepseek"],
  ["\u963f\u91cc\u4e91", "alibaba"],
  ["\u963f\u91cc\u5df4\u5df4", "alibaba"],
  ["\u901a\u4e49\u5343\u95ee", "alibaba"],
  ["\u901a\u4e49", "alibaba"],
  ["\u6708\u4e4b\u6697\u9762", "moonshot"],
  ["\u667a\u8c31", "zhipu"],
  ["\u667a\u8c31AI", "zhipu"],
  ["\u5b57\u8282\u8df3\u52a8", "bytedance"],
  ["\u8c46\u5305", "bytedance"],
  ["\u767e\u5ea6", "baidu"],
  ["\u817e\u8baf", "tencent"],
  ["\u79d1\u5927\u8baf\u98de", "iflytek"],
  ["\u9636\u8dc3\u661f\u8fb0", "stepfun"],
  ["\u767e\u5ddd", "baichuan"],
  ["\u96f6\u4e00\u4e07\u7269", "01ai"],
  ["\u5feb\u624b", "kuaishou"],
]
for (const [alias, id] of localizedAliases) {
  const vendor = registry.get(id)
  if (vendor) registry.set(normalize(alias), vendor)
}

export function modelVendor(model: Pick<PricingModel, 'vendor'>): Vendor {
  const raw = model.vendor?.trim()
  if (!raw) return { id: 'other', name: '' }
  return registry.get(normalize(raw)) ?? { id: `custom:${raw.toLowerCase()}`, name: raw }
}

// Send normalized aliases as bound server filters; custom vendors retain exact names.
export function vendorFilter(id: string): Record<string, string> {
  if (id === 'other') return { vendor: '' }
  if (id.startsWith('custom:')) return { vendor: id.slice(7) }
  const aliases = [...registry.entries()].filter(([, vendor]) => vendor.id === id).map(([alias]) => alias)
  return aliases.length ? { vendors: JSON.stringify(aliases) } : { vendor: id }
}

export const capabilityKeys = ['vision', 'tools', 'json', 'reasoning', 'audio', 'video', 'embedding', 'realtime', 'parallel_tools', 'structured_output', 'streaming', 'prompt_cache', 'system_prompt', 'temperature', 'web_search', 'computer_use'] as const
export function modelCapabilities(model: PricingModel) {
  return capabilityKeys.filter((key) => model.capabilities?.[key] === true)
}

export function nonnegative(raw: string | number | null | undefined): number | null {
  if (raw == null || (typeof raw === 'string' && raw.trim() === '')) return null
  const value = Number(raw)
  return Number.isFinite(value) && value >= 0 ? value : null
}

export type PriceField = 'input' | 'output' | 'cache' | 'cacheWrite' | 'audioIn' | 'audioOut' | 'imageIn' | 'call' | 'cache_write_5m' | 'cache_write_1h' | 'image_cache_read' | 'audio_cache_read' | 'image_cache_write' | 'audio_cache_write' | 'image_output'

// 统一返回 micro-USD。阶梯公式不能伪装成固定单价；缺失值与真实零价严格区分。
export function modelPrice(model: PricingModel, field: PriceField, factor: number | null, unit: TokenUnit = '1M'): number | null {
  if (factor === null || !Number.isFinite(factor) || factor < 0) return null
  if (field === 'call') {
    const price = nonnegative(model.per_call_price_micro)
    return model.mode === 'per_call' && price !== null ? price * factor : null
  }
  if (model.mode !== 'ratio') return null
  const base = nonnegative(model.model_ratio)
  if (base === null) return null
  const ratios: Record<Exclude<PriceField, 'call'>, Array<string | number | null>> = {
    input: [], output: [model.completion_ratio], cache: [model.cache_ratio],
    cacheWrite: [model.cache_write_ratio], audioIn: [model.audio_ratio],
    audioOut: [model.audio_ratio, model.audio_completion_ratio], imageIn: [model.image_ratio],
    cache_write_5m: [model.modality_ratios?.cache_write_5m ?? model.cache_write_ratio],
    cache_write_1h: [model.modality_ratios?.cache_write_1h ?? model.cache_write_ratio],
    image_cache_read: model.modality_ratios?.image_cache_read != null ? [model.modality_ratios.image_cache_read] : [model.image_ratio, model.cache_ratio],
    audio_cache_read: model.modality_ratios?.audio_cache_read != null ? [model.modality_ratios.audio_cache_read] : [model.audio_ratio, model.cache_ratio],
    image_cache_write: model.modality_ratios?.image_cache_write != null ? [model.modality_ratios.image_cache_write] : [model.image_ratio, model.cache_write_ratio],
    audio_cache_write: model.modality_ratios?.audio_cache_write != null ? [model.modality_ratios.audio_cache_write] : [model.audio_ratio, model.cache_write_ratio],
    image_output: [model.modality_ratios?.image_output ?? model.completion_ratio],
  }
  const priceBase = nonnegative(model.base_price_per_1m_micro ?? 2_000_000)
  if (priceBase === null || priceBase === 0) return null
  let price = base * factor * priceBase / (unit === '1M' ? 1 : 1000)
  for (const raw of ratios[field]) {
    const ratio = nonnegative(raw)
    if (ratio === null) return null
    price *= ratio
  }
  return Number.isFinite(price) ? price : null
}

export function isAvailable(model: PricingModel, group: string) {
  return group ? model.groups.includes(group) : model.groups.length > 0
}

export function compareModels(a: PricingModel, b: PricingModel, sort: string, factor: number | null, locale: string) {
  if (sort === 'input' || sort === 'output' || sort === 'context') {
    const av = sort === 'context' ? nonnegative(a.context_window) : modelPrice(a, sort, factor)
    const bv = sort === 'context' ? nonnegative(b.context_window) : modelPrice(b, sort, factor)
    // 按次与阶梯计费不参与 Token 单价排序；未知值始终置底。
    if (av === null && bv !== null) return 1
    if (av !== null && bv === null) return -1
    if (av !== null && bv !== null && av !== bv) return sort === 'context' ? bv - av : av - bv
  }
  return (a.display_name || a.model).localeCompare(b.display_name || b.model, locale, { numeric: true })
}

/// 上下文窗口按行业惯例写成 K / M tokens：2 的幂（65,536 / 1,048,576）按 1024 折算成 64K / 1M，
/// 其余（128,000 / 200,000）按 1000 折算成 128K / 200K，不够整的保留一位小数。
/// 此前用"13万 / 105万 / 65,536"混着写，128K 被四舍五入成 13 万，既不准也和同页其他模型对不齐；精确值留在悬停与详情里。
export function formatContextWindow(tokens: number): string {
  const powerOfTwo = Number.isInteger(tokens) && tokens > 0 && (tokens & (tokens - 1)) === 0 && tokens < 2 ** 31
  const base = powerOfTwo ? 1024 : 1000
  const compact = (divisor: number, suffix: string) => {
    const value = tokens / divisor
    return `${Number.isInteger(value) ? value : Number(value.toFixed(1))}${suffix}`
  }
  if (tokens >= base * base) return compact(base * base, 'M')
  if (tokens >= base) return compact(base, 'K')
  return String(tokens)
}
