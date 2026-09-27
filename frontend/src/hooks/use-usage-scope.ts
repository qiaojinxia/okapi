import { useEffect, useSyncExternalStore } from 'react'
import type { Scope } from '@/features/portal-overview/types'
import { getLoginMode, USAGE_SCOPE_STORAGE } from '@/lib/api'
import { useMe } from './use-auth'

const event = 'okapi:usage-scope'
function subscribe(callback: () => void) {
  window.addEventListener(event, callback)
  window.addEventListener('storage', callback)
  return () => {
    window.removeEventListener(event, callback)
    window.removeEventListener('storage', callback)
  }
}
function snapshot(): Scope | null {
  const value = localStorage.getItem(USAGE_SCOPE_STORAGE)
  return value === 'key' || value === 'user' ? value : null
}
function setScope(value: Scope) {
  localStorage.setItem(USAGE_SCOPE_STORAGE, value)
  window.dispatchEvent(new Event(event))
}

// 仅决定展示范围，不参与鉴权。旧登录记录通过同一账户的有效 web 会话兼容。
export function useUsageScope(requested?: Scope) {
  const me = useMe()
  const mode = getLoginMode()
  const accountLogin = mode === 'account' || (mode === null && me.data?.has_web_session === true)
  const saved = useSyncExternalStore(subscribe, snapshot)
  const scope: Scope = accountLogin ? 'user' : requested ?? saved ?? 'key'
  useEffect(() => { if (requested && !accountLogin) setScope(requested) }, [requested, accountLogin])
  const keyLabel = me.data?.key_name
    ? `${me.data.key_name} · ${me.data.key_prefix ?? `#${me.data.key_id}`}`
    : me.data ? `#${me.data.key_id}` : '—'
  return { scope, setScope, accountLogin, keyLabel, ready: mode !== null || !me.isPending }
}
