import i18n from 'i18next'
import { initReactI18next } from 'react-i18next'
import { ApiError } from '@/lib/api'
import { readLocal, rememberLocal } from '@/lib/storage'

export type Lang = 'zh-CN' | 'en'

const LANG_STORAGE = 'okapi.lang'

type Resources = Record<string, object>

// 每种语言拆成两块各自成 chunk：base 是全站通用的命名空间，首屏只下当前语言的这一块；
// admin 占语言包一大半、只有 /admin 下的页面用，由 /admin 路由的 beforeLoad 调 loadAdminCopy 再拉。
// 组合方式要与 scripts/locale-keys.mjs 保持一致（那道闸按它核对键集）。
const packs: Record<Lang, { base: () => Promise<Resources>; admin: () => Promise<object> }> = {
  'zh-CN': {
    base: async () => {
      const [{ default: zhCN }, { withModelMetaZh }] = await Promise.all([
        import('@/locales/zh-CN'),
        import('@/locales/model-editor-zh'),
      ])
      return withModelMetaZh(zhCN)
    },
    admin: async () => {
      const [{ default: admin }, { withModelAdminZh }] = await Promise.all([
        import('@/locales/zh-CN.admin'),
        import('@/locales/model-editor-zh'),
      ])
      return withModelAdminZh(admin)
    },
  },
  en: {
    base: async () => (await import('@/locales/en')).default,
    admin: async () => (await import('@/locales/en.admin')).default,
  },
}

let active: Lang = 'en'
let adminWanted = false

/// 把某种语言要用的包补齐：base 总要；admin 只在进过 /admin 之后要（切语言时一并补新语言的）。
async function ensurePacks(lang: Lang): Promise<void> {
  const pending: Promise<void>[] = []
  if (!i18n.hasResourceBundle(lang, 'common')) {
    pending.push(packs[lang].base().then((resources) => {
      for (const [ns, bundle] of Object.entries(resources)) i18n.addResourceBundle(lang, ns, bundle)
    }))
  }
  if (adminWanted && !i18n.hasResourceBundle(lang, 'admin')) {
    pending.push(packs[lang].admin().then((bundle) => { i18n.addResourceBundle(lang, 'admin', bundle) }))
  }
  await Promise.all(pending)
}

function initialLanguage(): Lang {
  const saved = readLocal(LANG_STORAGE)
  if (saved === 'zh-CN' || saved === 'en') return saved
  return navigator.language.startsWith('zh') ? 'zh-CN' : 'en'
}

/// 当前语言的通用语言包下载完才 resolve：调用方等它再挂 React，第一帧就不会露出原始键。
export async function initI18n(): Promise<typeof i18n> {
  active = initialLanguage()
  await i18n.use(initReactI18next).init({
    resources: {},
    lng: active,
    fallbackLng: 'en',
    defaultNS: 'common',
    interpolation: { escapeValue: false },
  })
  await ensurePacks(active)
  return i18n
}

/// /admin 路由的 beforeLoad 等它：admin 文案到位前不渲染管理端页面。
export async function loadAdminCopy(): Promise<void> {
  adminWanted = true
  await ensurePacks(active)
}

export async function switchLanguage(lang: Lang): Promise<void> {
  rememberLocal(LANG_STORAGE, lang)
  // 先改 active：切换途中进 /admin 的 loadAdminCopy 就会拉新语言的 admin 包
  active = lang
  await ensurePacks(lang)
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
