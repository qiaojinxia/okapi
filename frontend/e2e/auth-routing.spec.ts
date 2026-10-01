import { expect, test } from '@playwright/test'
import type { Page } from '@playwright/test'
import { fileURLToPath } from 'node:url'

const fixtureKey = 'auth-routing-fixture-not-a-real-secret'
const me = { user_id: 7, key_id: 1, role: 1, permissions: [], group: 'default', balance_micro: 0 }

async function prepare(page: Page, { key = null, mode = 'account' }: {
  key?: string | null; mode?: 'account' | 'key'
} = {}) {
  const calls: string[] = []
  await page.addInitScript(({ key, mode }) => {
    localStorage.setItem('okapi.lang', 'zh-CN')
    // Initialize once so reloads exercise the actual persisted sign-in/sign-out state.
    if (!sessionStorage.getItem('auth-routing-initialized')) {
      sessionStorage.setItem('auth-routing-initialized', 'true')
      if (key !== null) {
        localStorage.setItem('okapi.key', key)
        localStorage.setItem('okapi.login-mode', mode)
        localStorage.setItem('okapi.usage-scope', 'user')
      }
    }
  }, { key, mode })
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
      '/api/registration': { mode: 'open', email_verification: false },
      '/auth/oauth-providers': { providers: [] },
      '/api/notice': { notice: null },
      '/api/pricing': { models: [], groups: [] },
      '/auth/logout': { ok: true },
    }
    return route.fulfill({ json: responses[path] ?? { data: [] } })
  })
  return calls
}

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
    expect(calls).not.toContain('POST /auth/keys')
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
  await page.route('**/auth/keys', (route) => {
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
