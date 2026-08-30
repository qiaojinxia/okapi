import i18n from 'i18next'
import { initReactI18next } from 'react-i18next'
import zhCN from '@/locales/zh-CN'
import en from '@/locales/en'
import { ApiError } from '@/lib/api'

const LANG_STORAGE = 'okapi.lang'

export function initI18n(): typeof i18n {
  const saved = localStorage.getItem(LANG_STORAGE)
  const fallback = navigator.language.startsWith('zh') ? 'zh-CN' : 'en'
  void i18n.use(initReactI18next).init({
    resources: {
      'zh-CN': zhCN,
      en,
    },
    lng: saved ?? fallback,
    fallbackLng: 'en',
    defaultNS: 'common',
    interpolation: { escapeValue: false },
  })
  return i18n
}

export function switchLanguage(lang: 'zh-CN' | 'en'): void {
  localStorage.setItem(LANG_STORAGE, lang)
  void i18n.changeLanguage(lang)
}

/// 后端 error_code → 本地化文案（errors 命名空间，i18n 红线唯一出口）。
export function describeError(err: unknown): string {
  if (err instanceof ApiError) {
    const key = `errors:${err.code}`
    if (i18n.exists(key)) {
      return i18n.t(key, { param: err.param ?? '' })
    }
    const httpKey = `errors:http_${err.status}`
    if (i18n.exists(httpKey)) {
      return i18n.t(httpKey)
    }
    return i18n.t('errors:unknown', { code: err.code })
  }
  return i18n.t('errors:internal_error')
}
