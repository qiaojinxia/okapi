import { keepPreviousData, useQuery } from '@tanstack/react-query'
import { useDebounced } from '@/hooks/use-debounced'
import { apiFetch } from '@/lib/api'
import { qk } from '@/lib/query-keys'
import type { MetadataDraft } from './model-config'
import { CAPABILITY_KEYS } from './types'

/// 内置 LiteLLM 价目目录的建议（后端 `okapi_pricing::catalog`）。它是输入辅助：
/// 只在管理员点击后进入表单草稿，保存并发布之后才有价格生效。
export interface CatalogNote {
  code: string
  params?: Record<string, string>
}

export interface CatalogAlternative {
  key: string
  provider: string
  input_per_1m: string | null
  output_per_1m: string | null
}

export interface CatalogSuggestion {
  key: string
  provider: string
  matched: 'exact' | 'prefixed' | 'bare'
  mode: string
  kind: string | null
  billing: 'ratio' | 'per_call' | 'unsupported'
  input_per_1m: string | null
  output_per_1m: string | null
  cache_read_ratio: string | null
  cache_write_ratio: string | null
  cache_write_1h_ratio: string | null
  per_call_usd: string | null
  context_window: number | null
  max_output: number | null
  input_modalities: string[]
  output_modalities: string[]
  capabilities: Record<string, boolean>
  notes: CatalogNote[]
  deprecated: string | null
  source_url: string | null
  alternatives: CatalogAlternative[]
}

export interface CatalogSnapshot {
  dataset: string
  commit: string | null
  generated_at: string
  source: string
}

export interface CatalogHit {
  id: string
  provider: string
  mode: string
  kind: string | null
  input_per_1m: string | null
  output_per_1m: string | null
  listings: number
}

/// 与 `okapi_pricing::catalog` 的备注码一一对应；后端新增的码在这里补上文案之前不显示，免得露出原始键。
export const NOTE_CODES: readonly string[] = ['long_context', 'service_tiers', 'reasoning_price', 'audio_price', 'per_second', 'free', 'no_price']

const LOOKUP_DELAY_MS = 400
const SEARCH_DELAY_MS = 250

/// 按完整名字查目录（名字停顿后才发请求）。`settled` 表示结果对应的就是输入框里现在的名字。
export function useCatalogSuggestion(name: string) {
  const typed = name.trim()
  const term = useDebounced(typed, LOOKUP_DELAY_MS)
  const query = useQuery({
    queryKey: qk.catalogLookup(term),
    enabled: term !== '',
    retry: false,
    staleTime: 10 * 60_000,
    queryFn: ({ signal }) => apiFetch<{ suggestion: CatalogSuggestion | null; snapshot: CatalogSnapshot }>(
      `/admin/pricing/catalog/lookup?model=${encodeURIComponent(term)}`, { signal }),
  })
  return { query, settled: term === typed && term !== '' }
}

/// 名称联想：至少两个字符、停顿之后才查，翻页不闪烁。
export function useCatalogSearch(text: string, enabled: boolean) {
  const term = useDebounced(text.trim(), SEARCH_DELAY_MS)
  return useQuery({
    queryKey: qk.catalogSearch(term),
    enabled: enabled && term.length >= 2,
    retry: false,
    staleTime: 10 * 60_000,
    placeholderData: keepPreviousData,
    queryFn: ({ signal }) => apiFetch<{ data: CatalogHit[] }>(
      `/admin/pricing/catalog/search?q=${encodeURIComponent(term)}&limit=20`, { signal }),
  })
}

/// 填进表单的价格相对参考价的倍数；任何一边不是正数就没有可比性。
export function priceRatio(entered: string, reference: string | null | undefined): number | null {
  if (!reference || !/^\d+(?:\.\d+)?$/.test(entered.trim())) return null
  const a = Number(entered), b = Number(reference)
  return a > 0 && b > 0 && Number.isFinite(a / b) ? a / b : null
}

/// 差一倍以上或不到一半才提示：加价与折扣是常态，数量级错位（小数点、单位）才是要拦的。
export function deviates(ratio: number | null): ratio is number {
  return ratio !== null && (ratio >= 2 || ratio <= 0.5)
}

const MODALITIES = new Set(['text', 'image', 'audio', 'video'])
const MAX_LIMIT = 2_147_483_647

/// 目录规格并进当前元数据草稿：管理员已经填的一律不动，只补空项。
export function mergeCatalogSpecs(draft: MetadataDraft, suggestion: CatalogSuggestion): MetadataDraft {
  const known = (list: string[]) => list.filter((modality) => MODALITIES.has(modality))
  const limit = (current: string, value: number | null) =>
    current.trim() !== '' ? current : value !== null && value > 0 && value <= MAX_LIMIT ? String(value) : ''
  const declared = Object.fromEntries(CAPABILITY_KEYS
    .filter((key) => typeof suggestion.capabilities[key] === 'boolean')
    .map((key) => [key, suggestion.capabilities[key]]))
  return {
    ...draft,
    kind: draft.kind || suggestion.kind || '',
    input_modalities: draft.input_modalities.length > 0 ? draft.input_modalities : known(suggestion.input_modalities),
    output_modalities: draft.output_modalities.length > 0 ? draft.output_modalities : known(suggestion.output_modalities),
    capabilities: { ...declared, ...draft.capabilities },
    context_window: limit(draft.context_window, suggestion.context_window),
    max_output: limit(draft.max_output, suggestion.max_output),
  }
}
