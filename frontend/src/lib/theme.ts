import { useEffect, useState } from 'react'

/// 主题偏好：跟随系统 / 亮 / 暗。
///
/// 此前只在 `<html>` 上切一个 `.dark` 类且不持久化——刷新即回亮色，夜里排障的人
/// 每开一页要点一次。现在持久化到 localStorage，并在 `index.html` 的内联脚本里
/// 于首屏渲染前就把类挂好（避免亮→暗闪一下）。
export type ThemePreference = 'system' | 'light' | 'dark'

export const THEME_STORAGE = 'okapi.theme'

const listeners = new Set<() => void>()

export function getThemePreference(): ThemePreference {
  const v = localStorage.getItem(THEME_STORAGE)
  return v === 'light' || v === 'dark' ? v : 'system'
}

function systemDark(): boolean {
  return window.matchMedia('(prefers-color-scheme: dark)').matches
}

export function resolveTheme(pref: ThemePreference): 'light' | 'dark' {
  return pref === 'system' ? (systemDark() ? 'dark' : 'light') : pref
}

function apply(pref: ThemePreference): void {
  const root = document.documentElement
  // 切换瞬间给全局加一次过渡类，切完即摘——常驻会让所有 hover 变慢
  root.classList.add('theme-transition')
  root.classList.toggle('dark', resolveTheme(pref) === 'dark')
  window.setTimeout(() => root.classList.remove('theme-transition'), 250)
  for (const l of listeners) l()
}

export function setThemePreference(pref: ThemePreference): void {
  if (pref === 'system') localStorage.removeItem(THEME_STORAGE)
  else localStorage.setItem(THEME_STORAGE, pref)
  apply(pref)
}

/// 挂在应用根：跟随系统模式下，系统切换时页面同步；其他页面/组件通过
/// `useTheme` 订阅当前值。
export function useTheme(): {
  preference: ThemePreference
  resolved: 'light' | 'dark'
  setPreference: (p: ThemePreference) => void
  toggle: () => void
} {
  const [preference, setPref] = useState<ThemePreference>(() => getThemePreference())
  const [resolved, setResolved] = useState<'light' | 'dark'>(() =>
    document.documentElement.classList.contains('dark') ? 'dark' : 'light',
  )

  useEffect(() => {
    const sync = () => {
      setPref(getThemePreference())
      setResolved(document.documentElement.classList.contains('dark') ? 'dark' : 'light')
    }
    listeners.add(sync)
    const mq = window.matchMedia('(prefers-color-scheme: dark)')
    const onSystem = () => {
      if (getThemePreference() === 'system') apply('system')
    }
    mq.addEventListener('change', onSystem)
    return () => {
      listeners.delete(sync)
      mq.removeEventListener('change', onSystem)
    }
  }, [])

  return {
    preference,
    resolved,
    setPreference: setThemePreference,
    // 单击切换只在亮/暗之间翻转：多数人只想要"现在换成另一种"，
    // 系统跟随作为第三态放在菜单里
    toggle: () => setThemePreference(resolved === 'dark' ? 'light' : 'dark'),
  }
}
