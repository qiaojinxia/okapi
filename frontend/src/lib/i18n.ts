import i18n from 'i18next'
import { initReactI18next } from 'react-i18next'
import { ApiError } from '@/lib/api'
import { readLocal, rememberLocal } from '@/lib/storage'

export type Lang = 'zh-CN' | 'en'

const LANG_STORAGE = 'okapi.lang'

// 两种语言各自单独成块：首屏只下载当前语言，切换时再拉另一种。
// 组合方式要与 scripts/locale-keys.mjs 保持一致（那道闸按它核对键集）。
const packs: Record<Lang, () => Promise<Record<string, object>>> = {
  'zh-CN': async () => {
    const [{ default: zhCN }, { withModelEditorZh }] = await Promise.all([
      import('@/locales/zh-CN'),
      import('@/locales/model-editor-zh'),
    ])
    return withModelEditorZh(zhCN)
  },
  en: async () => (await import('@/locales/en')).default,
}

async function ensurePack(lang: Lang): Promise<void> {
  if (i18n.hasResourceBundle(lang, 'common')) return
  for (const [ns, resources] of Object.entries(await packs[lang]())) {
    i18n.addResourceBundle(lang, ns, resources)
  }
}

function initialLanguage(): Lang {
  const saved = readLocal(LANG_STORAGE)
  if (saved === 'zh-CN' || saved === 'en') return saved
  return navigator.language.startsWith('zh') ? 'zh-CN' : 'en'
}

/// 当前语言的语言包下载完才 resolve：调用方等它再挂 React，第一帧就不会露出原始键。
export async function initI18n(): Promise<typeof i18n> {
  const lng = initialLanguage()
  await i18n.use(initReactI18next).init({
    resources: {},
    lng,
    fallbackLng: 'en',
    defaultNS: 'common',
    interpolation: { escapeValue: false },
  })
  await ensurePack(lng)
  return i18n
}

export async function switchLanguage(lang: Lang): Promise<void> {
  rememberLocal(LANG_STORAGE, lang)
  await ensurePack(lang)
  await i18n.changeLanguage(lang)
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
