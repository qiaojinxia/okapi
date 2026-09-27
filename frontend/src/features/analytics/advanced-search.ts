import { oneOf } from '@/lib/search-params'

export const ADVANCED_KEYS = ['start_date', 'end_date', 'granularity', 'model_source', 'endpoint', 'upstream_endpoint', 'node', 'request_type', 'billing_type', 'stream', 'models', 'groups'] as const

export interface AdvancedSearch {
  start_date?: string
  end_date?: string
  granularity?: 'hour' | 'day'
  model_source?: 'billed' | 'requested' | 'upstream'
  endpoint?: string
  upstream_endpoint?: string
  node?: string
  request_type?: string
  billing_type?: string
  stream?: boolean
  models?: string[]
  groups?: string[]
}

function str(value: unknown): string | undefined {
  return typeof value === 'string' && value.trim() !== '' ? value.trim() : undefined
}

function choices(value: unknown): string[] | undefined {
  if (typeof value === 'string') { try { value = JSON.parse(value) } catch { return undefined } }
  return Array.isArray(value) && value.length <= 8 && value.every((v) => typeof v === 'string' && v.length > 0 && v.length <= 256) ? value : undefined
}

// 用量与质量趋势共用 URL 解析；空值显式清除，重置不会残留旧筛选。
export function advancedSearch(search: Record<string, unknown>): AdvancedSearch {
  return {
    start_date: str(search.start_date), end_date: str(search.end_date),
    granularity: oneOf(search.granularity, ['hour', 'day']),
    model_source: oneOf(search.model_source, ['billed', 'requested', 'upstream']),
    endpoint: str(search.endpoint), upstream_endpoint: str(search.upstream_endpoint), node: str(search.node),
    request_type: str(search.request_type), billing_type: str(search.billing_type),
    stream: search.stream === true || search.stream === 'true' ? true : search.stream === false || search.stream === 'false' ? false : undefined,
    models: choices(search.models), groups: choices(search.groups),
  }
}
