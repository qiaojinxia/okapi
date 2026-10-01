import type { ChatUsage } from './chat-stream'
import { modelPrice } from '@/features/public-pricing/catalog-data'
import type { PricingModel } from '@/features/public-pricing/types'

/// 一次回复的费用估算（micro-USD），口径与模型广场同一份价目：按次计费取单价，
/// 按量计费取「未缓存输入 × 输入价 + 缓存命中 × 缓存价 + 输出 × 输出价」。
/// 只是估算——个人系数、折扣与动态规则以用量日志的实扣为准，所以界面上写"约"。
/// 阶梯 / 自定义计价没有固定单价，返回 null（不显示，也不编一个数）。
export function estimateCost(model: PricingModel | undefined, factor: number | null, usage: ChatUsage | null | undefined): number | null {
  if (!model || factor === null) return null
  if (model.mode === 'per_call') return modelPrice(model, 'call', factor)
  if (model.mode !== 'ratio' || !usage) return null
  const input = modelPrice(model, 'input', factor)
  const output = modelPrice(model, 'output', factor)
  if (input === null || output === null) return null
  const cachedPrice = modelPrice(model, 'cache', factor) ?? input
  // prompt_tokens 含缓存命中部分（OpenAI 口径）；缓存读数异常大时按全部命中封顶
  const cached = Math.min(Math.max(usage.cached_tokens, 0), usage.prompt_tokens)
  return ((usage.prompt_tokens - cached) * input + cached * cachedPrice + usage.completion_tokens * output) / 1_000_000
}

/// 输出速度（tokens/s）：完成 token 数 ÷ 首字之后的耗时。耗时过短（< 50ms）时数值没有意义。
export function outputSpeed(completionTokens: number, ttftMs: number | undefined, durationMs: number | undefined): number | null {
  if (ttftMs === undefined || durationMs === undefined || completionTokens <= 0) return null
  const generating = durationMs - ttftMs
  return generating >= 50 ? completionTokens / (generating / 1000) : null
}
