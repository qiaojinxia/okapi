import { useQuery } from '@tanstack/react-query'
import { useCallback, useSyncExternalStore } from 'react'
import { apiFetch } from '@/lib/api'
import { qk } from '@/lib/query-keys'

/// 聊天预设 = 模型 + 系统提示词 + 采样参数（IMPLEMENTATION §11.39）。
export interface Preset {
  name: string
  model: string
  system: string
  temperature: number
  top_p: number
  /// null = 不传，让模型用自己的缺省。
  max_tokens: number | null
}

/// 站点预设（`settings.playground_presets` 经公开端点白名单收口后的形状）。
export interface SitePreset {
  name: string
  model: string
  system: string
  temperature: number | null
  max_tokens: number | null
  top_p: number | null
}

export const DEFAULT_TEMPERATURE = 1
export const DEFAULT_TOP_P = 1

/// 站点预设缺省的采样参数补齐为本地形状。
export function fromSitePreset(p: SitePreset): Preset {
  return {
    name: p.name,
    model: p.model,
    system: p.system,
    temperature: p.temperature ?? DEFAULT_TEMPERATURE,
    top_p: p.top_p ?? DEFAULT_TOP_P,
    max_tokens: p.max_tokens,
  }
}

export function useSitePresets() {
  return useQuery({
    queryKey: qk.playgroundPresets,
    queryFn: () => apiFetch<{ data: SitePreset[] }>('/api/playground/presets'),
    staleTime: 60_000,
  })
}

// 用户预设存 localStorage（按用户隔离，与引导状态同一存法）：试用台配置不值得一张表。
const storageKey = (userId: number) => `okapi.playground.${userId}`
const listeners = new Set<() => void>()
const MAX_PRESETS = 30

function isPreset(v: unknown): v is Preset {
  if (typeof v !== 'object' || v === null) return false
  const p = v as Record<string, unknown>
  return (
    typeof p.name === 'string' &&
    typeof p.model === 'string' &&
    typeof p.system === 'string' &&
    typeof p.temperature === 'number' &&
    typeof p.top_p === 'number' &&
    (p.max_tokens === null || typeof p.max_tokens === 'number')
  )
}

export function readPresets(userId: number): Preset[] {
  try {
    const raw = JSON.parse(localStorage.getItem(storageKey(userId)) ?? '[]') as unknown
    return Array.isArray(raw) ? raw.filter(isPreset) : []
  } catch {
    return []
  }
}

function writePresets(userId: number, presets: Preset[]): void {
  localStorage.setItem(storageKey(userId), JSON.stringify(presets.slice(0, MAX_PRESETS)))
  for (const l of listeners) l()
}

/// 同名覆盖（"保存"就是 upsert；用户改了参数再保存不该多出一条）。
export function upsertPreset(userId: number, preset: Preset): void {
  const rest = readPresets(userId).filter((p) => p.name !== preset.name)
  writePresets(userId, [preset, ...rest])
}

export function removePreset(userId: number, name: string): void {
  writePresets(userId, readPresets(userId).filter((p) => p.name !== name))
}

export function useUserPresets(userId: number | undefined) {
  const subscribe = useCallback((onChange: () => void) => {
    listeners.add(onChange)
    return () => listeners.delete(onChange)
  }, [])
  // 快照必须引用稳定：按序列化串缓存，内容不变就返回同一个数组
  const snapshot = useCallback(() => (userId === undefined ? '[]' : (localStorage.getItem(storageKey(userId)) ?? '[]')), [userId])
  const raw = useSyncExternalStore(subscribe, snapshot, () => '[]')
  return parseCached(raw)
}

let lastRaw = ''
let lastParsed: Preset[] = []
function parseCached(raw: string): Preset[] {
  if (raw === lastRaw) return lastParsed
  lastRaw = raw
  try {
    const v = JSON.parse(raw) as unknown
    lastParsed = Array.isArray(v) ? v.filter(isPreset) : []
  } catch {
    lastParsed = []
  }
  return lastParsed
}
