import { metadataDraft, type MetadataDraft } from './model-config'
import type { CAPABILITY_KEYS, MODEL_KINDS } from './types'

type Capability = (typeof CAPABILITY_KEYS)[number]
type Modality = 'text' | 'image' | 'audio' | 'video'
export type PresetNotice = 'gpt6' | 'gpt61Tools' | 'gpt6Tools' | 'gemini' | 'geminiLegacy' | 'geminiPro'
  | 'geminiPromotion' | 'imagePricing' | 'deepseek' | 'regional' | 'kimiCache' | 'partial'

export interface ModelPreset {
  id: string
  /** Other official IDs of the same snapshot (e.g. the dated Claude API ID behind an alias). */
  aliases?: readonly string[]
  displayName: string
  vendor: string
  kind: (typeof MODEL_KINDS)[number]
  inputs: readonly Modality[]
  outputs: readonly Modality[]
  capabilities: Readonly<Partial<Record<Capability, boolean>>>
  contextWindow?: number
  maxOutput?: number
  source: string
  checkedAt: string
  pricingSource?: string
  referencePrice?: { input: string; output: string; validUntil?: string }
  cache?: { read: string; write?: string; write5m?: string; write1h?: string }
  cacheTtls?: readonly ('5m' | '1h')[]
  notices?: readonly PresetNotice[]
}

// Curated facts, not runtime name inference or a complete upstream billing model.
// Sources were checked 2026-09-30. Missing capabilities/limits are deliberately unknown.
// Do not extend aliases by prefixes: snapshots and similarly named models may differ.
const checkedAt = '2026-09-30'
const openai = (id: string, displayName: string, contextWindow: number, maxOutput: number,
  input: string, output: string, read: string, reasoning = false): ModelPreset => ({
  id, displayName, vendor: 'OpenAI', kind: 'chat', inputs: ['text', 'image'], outputs: ['text'],
  capabilities: { vision: true, tools: true, structured_output: true, reasoning, streaming: true, prompt_cache: true, audio: false, video: false },
  contextWindow, maxOutput, source: `https://developers.openai.com/api/docs/models/${id}`, checkedAt,
  referencePrice: { input, output }, cache: { read },
})
const claude = (id: string, displayName: string, contextWindow: number, maxOutput: number,
  input: string, output: string, read: string, source: string, aliases?: readonly string[]): ModelPreset => ({
  id, aliases, displayName, vendor: 'Anthropic', kind: 'chat', inputs: ['text', 'image'], outputs: ['text'],
  capabilities: { vision: true, tools: true, reasoning: true, prompt_cache: true },
  contextWindow, maxOutput, source, checkedAt, referencePrice: { input, output },
  pricingSource: 'https://platform.claude.com/docs/en/build-with-claude/prompt-caching',
  cache: { read, write: '1.25', write5m: '1.25', write1h: '2' }, cacheTtls: ['5m', '1h'],
})
const gemini = (id: string, displayName: string): ModelPreset => ({
  id, displayName, vendor: 'Google', kind: 'chat', inputs: ['text', 'image', 'audio', 'video'], outputs: ['text'],
  capabilities: { vision: true, tools: true, structured_output: true, reasoning: true, audio: true, video: true,
    prompt_cache: true, web_search: true, realtime: false },
  contextWindow: 1_048_576, maxOutput: 65_536, checkedAt,
  source: `https://ai.google.dev/gemini-api/docs/models/${id}`,
  pricingSource: 'https://ai.google.dev/gemini-api/docs/pricing', cache: { read: '0.1' }, notices: ['gemini'],
})
const gpt6 = (preset: ModelPreset, notice?: 'gpt61Tools' | 'gpt6Tools'): ModelPreset => ({
  ...preset, cache: { ...preset.cache!, write: '1.25' }, notices: ['gpt6', ...(notice ? [notice] : [])],
})
const qwen: ModelPreset = {
  id: 'qwen3.6-plus', displayName: 'Qwen 3.6 Plus', vendor: 'Alibaba', kind: 'chat',
  inputs: ['text', 'image', 'video'], outputs: ['text'],
  capabilities: { vision: true, video: true, tools: true, structured_output: true, prompt_cache: true, web_search: true, reasoning: true },
  contextWindow: 1_000_000, maxOutput: 65_536, checkedAt,
  source: 'https://www.alibabacloud.com/help/en/model-studio/qwen3-6-plus', notices: ['regional', 'partial'],
}

export const MODEL_PRESETS: readonly ModelPreset[] = [
  gpt6(openai('gpt-6.1-sol', 'GPT-6.1 Sol', 1_050_000, 128_000, '2', '10', '0.05', true), 'gpt61Tools'),
  gpt6(openai('gpt-6-astra', 'GPT-6 Astra', 1_050_000, 128_000, '10', '50', '0.1', true)),
  gpt6(openai('gpt-6-sol', 'GPT-6 Sol', 1_050_000, 128_000, '2', '10', '0.1', true), 'gpt6Tools'),
  gpt6(openai('gpt-6-luna', 'GPT-6 Luna', 1_050_000, 128_000, '0.1', '0.5', '0.1', true), 'gpt6Tools'),
  openai('gpt-4.1', 'GPT-4.1', 1_047_576, 32_768, '2', '8', '0.25'),
  openai('gpt-4.1-mini', 'GPT-4.1 mini', 1_047_576, 32_768, '0.4', '1.6', '0.25'),
  openai('gpt-4o', 'GPT-4o', 128_000, 16_384, '2.5', '10', '0.5'),
  openai('gpt-4o-mini', 'GPT-4o mini', 128_000, 16_384, '0.15', '0.6', '0.5'),
  // Rechecked 2026-10-06 against platform.claude.com pricing and per-model pages:
  // cache read 0.1x except Fable 5.1 (0.025x) and Opus 5.5 (0.05x); writes 1.25x (5m) / 2x (1h).
  claude('claude-fable-5-1', 'Claude Fable 5.1', 1_000_000, 128_000, '10', '50', '0.025', 'https://platform.claude.com/docs/en/models/fable-5-1/overview'),
  claude('claude-opus-5-5', 'Claude Opus 5.5', 1_000_000, 128_000, '4', '20', '0.05', 'https://platform.claude.com/docs/en/models/opus-5-5/overview'),
  claude('claude-sonnet-5-5', 'Claude Sonnet 5.5', 1_000_000, 128_000, '2', '10', '0.1', 'https://platform.claude.com/docs/en/models/sonnet-5-5/overview'),
  claude('claude-haiku-4-5', 'Claude Haiku 4.5', 200_000, 64_000, '1', '5', '0.1', 'https://platform.claude.com/docs/en/models/haiku-4-5/overview', ['claude-haiku-4-5-20251001']),
  claude('claude-fable-5', 'Claude Fable 5', 1_000_000, 128_000, '10', '50', '0.1', 'https://platform.claude.com/docs/en/models/fable-5/overview'),
  claude('claude-opus-5', 'Claude Opus 5', 1_000_000, 128_000, '5', '25', '0.1', 'https://platform.claude.com/docs/en/models/opus-5/overview'),
  claude('claude-sonnet-5', 'Claude Sonnet 5', 1_000_000, 128_000, '2', '10', '0.1', 'https://platform.claude.com/docs/en/models/sonnet-5/overview'),
  claude('claude-opus-4-8', 'Claude Opus 4.8', 1_000_000, 128_000, '5', '25', '0.1', 'https://platform.claude.com/docs/en/models/opus-4-8/overview'),
  claude('claude-opus-4-7', 'Claude Opus 4.7', 1_000_000, 128_000, '5', '25', '0.1', 'https://platform.claude.com/docs/en/models/opus-4-7/overview'),
  claude('claude-opus-4-6', 'Claude Opus 4.6', 1_000_000, 128_000, '5', '25', '0.1', 'https://platform.claude.com/docs/en/models/opus-4-6/overview'),
  claude('claude-sonnet-4-6', 'Claude Sonnet 4.6', 1_000_000, 128_000, '3', '15', '0.1', 'https://platform.claude.com/docs/en/models/sonnet-4-6/overview'),
  claude('claude-opus-4-5', 'Claude Opus 4.5', 200_000, 64_000, '5', '25', '0.1', 'https://platform.claude.com/docs/en/models/opus-4-5/overview', ['claude-opus-4-5-20251101']),
  claude('claude-sonnet-4-5', 'Claude Sonnet 4.5', 200_000, 64_000, '3', '15', '0.1', 'https://platform.claude.com/docs/en/models/sonnet-4-5/overview', ['claude-sonnet-4-5-20250929']),
  { ...gemini('gemini-3.8-flash', 'Gemini 3.8 Flash'), referencePrice: { input: '0.75', output: '3.75', validUntil: '2026-12-31' },
    notices: ['gemini', 'geminiPromotion'] },
  { ...gemini('gemini-3.1-flash-lite', 'Gemini 3.1 Flash-Lite'), referencePrice: { input: '0.25', output: '1.5' } },
  { ...gemini('gemini-2.5-pro', 'Gemini 2.5 Pro'), referencePrice: { input: '1.25', output: '10' }, notices: ['gemini', 'geminiPro', 'geminiLegacy'] },
  { ...gemini('gemini-2.5-flash', 'Gemini 2.5 Flash'), referencePrice: { input: '0.3', output: '2.5' }, notices: ['gemini', 'geminiLegacy'] },
  { id: 'gemini-3.1-flash-image', displayName: 'Gemini 3.1 Flash Image', vendor: 'Google', kind: 'image_generation',
    inputs: ['text', 'image', 'video'], outputs: ['text', 'image'], contextWindow: 131_072, maxOutput: 32_768,
    capabilities: { vision: true, video: true, tools: false, structured_output: false, reasoning: true, prompt_cache: false, realtime: false, web_search: true },
    checkedAt, source: 'https://ai.google.dev/gemini-api/docs/models/gemini-3.1-flash-image', notices: ['imagePricing'] },
  { id: 'deepseek-flash', displayName: 'DeepSeek V4.1 Flash', vendor: 'DeepSeek', kind: 'chat',
    inputs: ['text', 'image'], outputs: ['text'], contextWindow: 1_000_000, maxOutput: 393_216,
    capabilities: { vision: true, tools: true, json: true, reasoning: true, prompt_cache: true },
    checkedAt, source: 'https://api-docs.deepseek.com/quick_start/pricing/', cache: { read: '0.02' }, notices: ['deepseek'],
    referencePrice: { input: '0.3', output: '1.2' } },
  // Output limits use https://api-docs.deepseek.com/api/create-chat-completion/ (384K = 393216).
  // Pro's 1/30 cache rate is rounded to the engine's six-place ratio precision.
  { id: 'deepseek-v4-pro', displayName: 'DeepSeek V4 Pro', vendor: 'DeepSeek', kind: 'chat',
    inputs: ['text'], outputs: ['text'], contextWindow: 1_000_000, maxOutput: 393_216,
    capabilities: { vision: false, tools: true, json: true, reasoning: true, prompt_cache: true },
    checkedAt, source: 'https://api-docs.deepseek.com/quick_start/pricing/', cache: { read: '0.033333' }, notices: ['deepseek'],
    referencePrice: { input: '1.32', output: '3.96' } },
  qwen,
  { ...qwen, id: 'qwen3.6-plus-2026-04-02', displayName: 'Qwen 3.6 Plus · 2026-04-02',
    capabilities: { ...qwen.capabilities, prompt_cache: false } },
  { id: 'kimi-k3', displayName: 'Kimi K3', vendor: 'Moonshot', kind: 'chat',
    inputs: ['text', 'image'], outputs: ['text'], contextWindow: 1_000_000,
    capabilities: { vision: true, tools: true, reasoning: true, structured_output: true, prompt_cache: true },
    checkedAt, source: 'https://platform.kimi.ai/docs/guide/kimi-k3-quickstart',
    pricingSource: 'https://platform.kimi.ai/docs/pricing/chat', cacheTtls: ['5m', '1h'], notices: ['kimiCache', 'partial'] },
  { id: 'kimi-k2.6', displayName: 'Kimi K2.6', vendor: 'Moonshot', kind: 'chat', inputs: ['text', 'image'], outputs: ['text'],
    contextWindow: 256_000, capabilities: { vision: true, reasoning: true }, checkedAt,
    source: 'https://platform.kimi.ai/docs/models', notices: ['partial'] },
]

/** Exact ID only. Unknown aliases must stay custom; never guess from vendor prefixes. */
export function findModelPreset(id: string): ModelPreset | undefined {
  const name = id.trim()
  return MODEL_PRESETS.find((preset) => preset.id === name || preset.aliases?.includes(name))
}

export function presetMetadata(preset: ModelPreset): MetadataDraft {
  return { ...metadataDraft(), display_name: preset.displayName, vendor: preset.vendor, kind: preset.kind,
    input_modalities: [...preset.inputs], output_modalities: [...preset.outputs], capabilities: { ...preset.capabilities },
    context_window: String(preset.contextWindow ?? ''), max_output: String(preset.maxOutput ?? '') }
}

/** Replace preset-owned cache lanes only; preserve base prices, modality rates and routing. */
export function presetCache(preset: ModelPreset) {
  return {
    cache_ratio: preset.cache?.read ?? '1', cache_write_ratio: preset.cache?.write ?? '1',
    ttl: Object.fromEntries([
      ['cache_write_5m', preset.cache?.write5m], ['cache_write_1h', preset.cache?.write1h],
    ].filter(([, value]) => value !== undefined)) as Record<string, string>,
  }
}

// Expired promotional reference prices cannot be applied by a future build at runtime.
export function referencePriceAvailable(preset: ModelPreset, date = new Date().toISOString().slice(0, 10)) {
  return Boolean(preset.referencePrice && (!preset.referencePrice.validUntil || date <= preset.referencePrice.validUntil))
}
