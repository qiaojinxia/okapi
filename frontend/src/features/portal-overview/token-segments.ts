/// Token 用量拆成互斥的几段：运营概览的 Token 构成环形图在用。

export interface Segment {
  key: 'input' | 'cached' | 'write' | 'output' | 'reasoning'
  value: number
  className: string
}

/// 把 OpenAI 口径的四个 usage 字段拆成互斥四段：
/// cached ⊂ prompt、reasoning ⊂ completion，直接画会把缓存和推理各算两遍。
export function segments(t: {
  prompt_tokens: number
  cached_tokens: number
  cache_write_tokens?: number | null
  recorded_cache_write_tokens?: number | null
  completion_tokens: number
  reasoning_tokens: number
}): Segment[] {
  const cached = Math.min(t.cached_tokens, t.prompt_tokens)
  const reasoning = Math.min(t.reasoning_tokens, t.completion_tokens)
  const writes = Math.min(t.recorded_cache_write_tokens ?? t.cache_write_tokens ?? 0, t.prompt_tokens - cached)
  return [
    { key: 'input', value: t.prompt_tokens - cached - writes, className: 'bg-primary' },
    { key: 'cached', value: cached, className: 'bg-success' },
    { key: 'write', value: writes, className: 'bg-chart-5' },
    { key: 'output', value: t.completion_tokens - reasoning, className: 'bg-warning' },
    { key: 'reasoning', value: reasoning, className: 'bg-muted-foreground' },
  ]
}
