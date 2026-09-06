import { useQuery } from '@tanstack/react-query'
import { createContext, useContext, useSyncExternalStore } from 'react'
import { useMe } from '@/hooks/use-auth'
import { apiFetch } from '@/lib/api'
import { qk } from '@/lib/query-keys'

export const GUIDE_STEPS = ['key', 'model', 'connect', 'call'] as const
export type GuideStep = (typeof GUIDE_STEPS)[number]

interface KeyBrief {
  used_micro: number
  requests: number
  last_used_at: string | null
}

/// 引导进度不靠"点过没点过"，而按账户真实状态推导：有密钥 → 第 1 步完成；
/// 任何一把密钥调用过（`last_used_at` / 累计消费 / 请求数非零）→ 后三步一并完成——
/// 能成功调用就意味着模型选了、客户端也配好了。点击式打卡换个浏览器就丢，
/// 而"有没有调过"这个事实后端一直记着。
export function useGuideProgress(): {
  loading: boolean
  keyCount: number
  done: Record<GuideStep, boolean>
  completed: number
} {
  const keys = useQuery({
    queryKey: qk.keysSummary,
    queryFn: () => apiFetch<{ data: KeyBrief[]; total: number }>('/api/me/keys?limit=50&offset=0'),
    staleTime: 30_000,
  })
  const rows = keys.data?.data ?? []
  const keyCount = keys.data?.total ?? rows.length
  const called = rows.some((k) => k.last_used_at !== null || k.used_micro > 0 || k.requests > 0)
  const done: Record<GuideStep, boolean> = { key: keyCount > 0, model: called, connect: called, call: called }
  return {
    loading: keys.isPending,
    keyCount,
    done,
    completed: GUIDE_STEPS.filter((step) => done[step]).length,
  }
}

const storageKey = (userId: number) => `okapi.guide.${userId}`
const listeners = new Set<() => void>()

/// "不再显示"按用户记忆：同一台电脑上换账号登录（合作商 / 员工）各自有各自的引导状态。
export function dismissGuide(userId: number): void {
  localStorage.setItem(storageKey(userId), 'dismissed')
  for (const l of listeners) l()
}

export function useGuideDismissed(): boolean {
  const me = useMe()
  const userId = me.data?.user_id
  return useSyncExternalStore(
    (onChange) => {
      listeners.add(onChange)
      return () => listeners.delete(onChange)
    },
    // 用户信息未回来前当作已关闭：宁可晚一瞬出现，也不要闪一下再消失
    () => (userId === undefined ? true : localStorage.getItem(storageKey(userId)) === 'dismissed'),
  )
}

export interface GuideRequest {
  /// 刚创建的密钥明文——只有从"密钥已创建"回执打开时才有；其余入口用占位符。
  apiKey?: string
}

/// 门户外壳提供：任何页面都能把引导抽屉拉出来（总览卡、顶栏 ?、密钥页）。
export const GuideContext = createContext<{ open: (req?: GuideRequest) => void }>({ open: () => undefined })

export function useGuide() {
  return useContext(GuideContext)
}
