import { expect, test } from '@playwright/test'
import type { Page } from '@playwright/test'
import { fileURLToPath } from 'node:url'

const fixtureKey = 'auth-routing-fixture-not-a-real-secret'
const me = { user_id: 7, key_id: 1, role: 1, permissions: [], group: 'default', balance_micro: 0 }

async function prepare(page: Page, { key = null, mode = 'account', lang = 'zh-CN', theme = 'light' }: {
  key?: string | null; mode?: 'account' | 'key'; lang?: 'zh-CN' | 'en'; theme?: 'light' | 'dark'
} = {}) {
  const calls: string[] = []
  await page.addInitScript(({ key, mode, lang, theme }) => {
    localStorage.setItem('okapi.lang', lang)
    localStorage.setItem('okapi.theme', theme)
    // Initialize once so reloads exercise the actual persisted sign-in/sign-out state.
    if (!sessionStorage.getItem('auth-routing-initialized')) {
      sessionStorage.setItem('auth-routing-initialized', 'true')
      if (key !== null) {
        localStorage.setItem('okapi.key', key)
        localStorage.setItem('okapi.login-mode', mode)
        localStorage.setItem('okapi.usage-scope', 'user')
      }
    }
  }, { key, mode, lang, theme })
  await page.route('**/*', (route) => {
    const request = route.request(), path = new URL(request.url()).pathname
    if (request.isNavigationRequest()) return route.fulfill({
      path: fileURLToPath(new URL('../dist/index.html', import.meta.url)), contentType: 'text/html',
    })
    if (!/^\/(api|auth|admin)\//.test(path)) return route.continue()
    calls.push(`${request.method()} ${path}`)
    const responses: Record<string, unknown> = {
      '/api/me': me,
      '/api/setup/status': { needs_setup: false },
      '/api/registration': {
        mode: 'open', email_verification: false, allowed_domains: [],
        new_user_credit_micro: 0, invitee_credit_micro: 0,
      },
      '/auth/oauth-providers': { providers: [] },
      '/api/notice': { notice: null },
      '/api/pricing': { models: [], groups: [] },
      '/auth/logout': { ok: true },
    }
    return route.fulfill({ json: responses[path] ?? { data: [] } })
  })
  return calls
}

for (const { width, lang, theme } of [
  { width: 360, lang: 'zh-CN', theme: 'light' },
  { width: 360, lang: 'en', theme: 'dark' },
  { width: 1366, lang: 'zh-CN', theme: 'light' },
  { width: 1366, lang: 'en', theme: 'dark' },
] as const) {
  test(`登录辅助操作 ${width}px ${lang} ${theme}：同排垂直居中，左右与表单对齐`, async ({ page }, testInfo) => {
    await page.setViewportSize({ width, height: 900 })
    await prepare(page, { lang, theme })
    await page.goto('/')
    const help = page.locator('[data-slot="auth-help"]')
    await expect(help.getByRole('button')).toBeVisible()
    await expect(help.getByRole('link')).toBeVisible()
    await page.evaluate(() => document.fonts.ready)
    // Wait for the entry animation before comparing rectangles across different elements.
    await page.locator('main').evaluate(async (el) => {
      await Promise.all(el.getAnimations({ subtree: true }).map((animation) => animation.finished))
    })
    const input = await page.locator('#email').boundingBox()
    const submit = await page.locator('form button[type="submit"]').boundingBox()
    const register = await help.getByRole('button').boundingBox()
    const forgot = await help.getByRole('link').boundingBox()
    if (!input || !submit || !register || !forgot) throw new Error('Missing login controls')
    expect(Math.abs(register.x - input.x)).toBeLessThanOrEqual(1)
    expect(Math.abs(forgot.x + forgot.width - input.x - input.width)).toBeLessThanOrEqual(1)
    expect(Math.abs(register.y + register.height / 2 - forgot.y - forgot.height / 2)).toBeLessThanOrEqual(1)
    expect(forgot.x - register.x - register.width).toBeGreaterThanOrEqual(15)
    expect(Math.min(register.y, forgot.y) - submit.y - submit.height).toBeGreaterThanOrEqual(19)
    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(width)
    if (width < 1024) {
      const footer = page.locator('main').getByRole('link', { name: lang === 'en' ? 'Model catalog' : '模型广场' })
      await expect(footer).toBeVisible()
      const box = await footer.boundingBox()
      if (!box) throw new Error('Missing model catalog footer')
      expect(Math.abs(box.x + box.width / 2 - input.x - input.width / 2)).toBeLessThanOrEqual(1)
      expect(box.y - Math.max(register.y + register.height, forgot.y + forgot.height)).toBeGreaterThanOrEqual(39)
    }
    await page.screenshot({ path: testInfo.outputPath('login-alignment.png'), fullPage: true })
    await help.getByRole('button').click()
    await expect(page.locator('#reg-email')).toBeVisible()
    await expect(help.getByRole('link')).toHaveCount(0)
    await help.getByRole('button').click()
    await expect(page.locator('#email')).toBeVisible()
    await expect(help.getByRole('link')).toBeVisible()
    await page.getByRole('button', { name: 'API Key', exact: true }).click()
    await expect(page.locator('#key')).toBeVisible()
    await expect(help.getByRole('link')).toHaveCount(0)
  })
}

test('第三方登录下辅助操作仍在同一行，找回密码保留已填邮箱', async ({ page }) => {
  await prepare(page)
  await page.route('**/auth/oauth-providers', (route) => route.fulfill({ json: { providers: ['github'] } }))
  await page.goto('/')
  await expect(page.getByRole('button', { name: '使用 github 登录' })).toBeVisible()
  const help = page.locator('[data-slot="auth-help"]')
  await expect(help.getByRole('button', { name: '还没有账号？立即注册' })).toBeVisible()
  await page.locator('#email').fill('who@ok.test')
  await help.getByRole('link', { name: '忘记密码？' }).click()
  await expect(page).toHaveURL(/\/forgot-password\?email=who(%40|@)ok\.test$/)
  await expect(page.locator('#forgot-email')).toHaveValue('who@ok.test')
})

test('未登录访问入口保留登录表单，不发身份检查', async ({ page }) => {
  const calls = await prepare(page)
  await page.goto('/')
  await expect(page.getByRole('button', { name: '登录', exact: true })).toBeVisible()
  expect(calls).not.toContain('GET /api/me')
})

for (const mode of ['account', 'key'] as const) {
  test(`${mode} 有效登录访问入口自动进入门户，刷新后仍保持登录`, async ({ page }) => {
    const calls = await prepare(page, { key: fixtureKey, mode })
    await page.goto('/')
    await expect(page).toHaveURL(/\/portal$/)
    await expect(page.getByRole('button', { name: '登录', exact: true })).toHaveCount(0)
    expect(calls).toContain('GET /api/me')
    expect(calls).not.toContain('POST /auth/session-key')
    await page.goto('/')
    await expect(page).toHaveURL(/\/portal$/)
    await page.reload()
    await expect(page).toHaveURL(/\/portal$/)
    expect(await page.evaluate(() => localStorage.getItem('okapi.key'))).toBe(fixtureKey)
  })
}

test('检查登录状态时不闪出登录表单', async ({ page }) => {
  await prepare(page, { key: fixtureKey })
  let release!: () => void
  const gate = new Promise<void>((resolve) => { release = resolve })
  await page.route('**/api/me', async (route) => {
    await gate
    await route.fulfill({ json: me })
  })
  await page.goto('/')
  await expect(page.getByRole('status')).toContainText('加载中')
  await expect(page.getByRole('button', { name: '登录', exact: true })).toHaveCount(0)
  release()
  await expect(page).toHaveURL(/\/portal$/)
})

for (const code of ['invalid_api_key', 'key_disabled']) {
  test(`${code} 失效凭证清理本地登录状态，允许重新登录且没有跳转循环`, async ({ page }) => {
    await prepare(page, { key: fixtureKey })
    let attempts = 0
    await page.route('**/api/me', (route) => {
      attempts++
      return route.fulfill({ status: 401, json: { error: { code } } })
    })
    await page.goto('/')
    await expect(page.getByRole('button', { name: '登录', exact: true })).toBeVisible()
    expect(await page.evaluate(() => [
      localStorage.getItem('okapi.key'), localStorage.getItem('okapi.login-mode'),
      localStorage.getItem('okapi.usage-scope'),
    ])).toEqual([null, null, null])
    expect(attempts).toBe(1)
    await page.reload()
    await expect(page.getByRole('button', { name: '登录', exact: true })).toBeVisible()
    expect(attempts).toBe(1)
  })
}

test('服务暂时失败保留登录凭证，重试成功后重定向', async ({ page }) => {
  await prepare(page, { key: fixtureKey })
  let unavailable = true
  await page.route('**/api/me', (route) => unavailable
    ? route.fulfill({ status: 503, json: { error: { code: 'internal_error' } } })
    : route.fulfill({ json: me }))
  await page.goto('/')
  await expect(page.getByRole('alert')).toContainText('服务内部错误')
  await expect(page.getByRole('button', { name: '登录', exact: true })).toHaveCount(0)
  expect(await page.evaluate(() => localStorage.getItem('okapi.key'))).toBe(fixtureKey)
  unavailable = false
  await page.getByRole('button', { name: '重试', exact: true }).click()
  await expect(page).toHaveURL(/\/portal$/)
})

test('网络断开不会清除登录凭证', async ({ page }) => {
  await prepare(page, { key: fixtureKey })
  await page.route('**/api/me', (route) => route.abort('failed'))
  await page.goto('/')
  await expect(page.getByRole('alert')).toBeVisible()
  expect(await page.evaluate(() => localStorage.getItem('okapi.key'))).toBe(fixtureKey)
  await expect(page.getByRole('button', { name: '登录', exact: true })).toHaveCount(0)
})

test('主动退出后可回登录页，不被反向守卫拉回门户', async ({ page }) => {
  const calls = await prepare(page, { key: fixtureKey })
  await page.goto('/')
  await expect(page).toHaveURL(/\/portal$/)
  await page.getByRole('button', { name: '退出登录', exact: true }).click()
  await expect(page).toHaveURL(/\/$/)
  await expect(page.getByRole('button', { name: '登录', exact: true })).toBeVisible()
  expect(calls).toContain('POST /auth/logout')
  await page.reload()
  await expect(page.getByRole('button', { name: '登录', exact: true })).toBeVisible()
})

test('OAuth 回调先兑换新会话，不被已有凭证的重定向抢走', async ({ page }) => {
  await prepare(page, { key: fixtureKey })
  const exchanges: unknown[] = []
  await page.route('**/auth/session-key', (route) => {
    exchanges.push(route.request().postDataJSON())
    return route.fulfill({ json: { api_key: 'oauth-routing-fixture', key_id: 2 } })
  })
  await page.goto('/?oauth=done')
  await expect(page).toHaveURL(/\/portal$/)
  expect(exchanges).toEqual([{ name: 'oauth' }])
  expect(await page.evaluate(() => localStorage.getItem('okapi.key'))).toBe('oauth-routing-fixture')
})

test('API Key 登录后浏览器返回不能再次停在登录页', async ({ page }) => {
  await prepare(page)
  await page.goto('/pricing')
  await page.goto('/')
  await page.getByRole('button', { name: 'API Key', exact: true }).click()
  await page.locator('#key').fill(fixtureKey)
  await page.getByRole('button', { name: '登录', exact: true }).click()
  await expect(page).toHaveURL(/\/portal$/)
  await page.goBack()
  await expect(page).toHaveURL(/\/portal$/)
  await expect(page.getByRole('button', { name: '登录', exact: true })).toHaveCount(0)
  await page.goBack()
  await expect(page).toHaveURL(/\/pricing$/)
})

test('没有任何管理权限的账号进 /admin 回门户，而不是对着一屏 403', async ({ page }) => {
  await prepare(page, { key: fixtureKey })
  await page.goto('/admin/channels')
  await expect(page).toHaveURL(/\/portal$/)
})
