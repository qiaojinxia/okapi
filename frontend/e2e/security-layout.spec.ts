import { expect, test } from '@playwright/test'
import type { Page } from '@playwright/test'
import { fileURLToPath } from 'node:url'

async function prepare(page: Page, { language = 'zh-CN', devices = 10, empty = false, notice = false } = {}) {
  await page.addInitScript((language) => {
    localStorage.setItem('okapi.key', 'security-layout-fixture')
    localStorage.setItem('okapi.lang', language)
    localStorage.setItem('okapi.theme', language === 'en' ? 'dark' : 'light')
  }, language)
  await page.emulateMedia({ reducedMotion: 'reduce' })
  await page.route('**/*', (route) => {
    const request = route.request(), path = new URL(request.url()).pathname
    if (request.isNavigationRequest()) return route.fulfill({ path: fileURLToPath(new URL('../dist/index.html', import.meta.url)), contentType: 'text/html' })
    if (!/^\/(api|auth|admin)\//.test(path)) return route.continue()
    expect(request.method(), '布局回归不得更改真实会话或两步验证').toBe('GET')
    const responses: Record<string, unknown> = {
      '/api/me': { user_id: 1, key_id: 1, role: 1, permissions: [], balance_micro: 49999100, group: 'vip' },
      '/api/notice': { notice: notice ? { title: 'Test notice', body: 'This banner also occupies part of the available viewport.', level: 'info', updated_at: '2026-09-28T00:00:00Z' } : null },
      '/api/me/sessions': { limit: 10, data: empty ? [] : Array.from({ length: devices }, (_, i) => ({
        sid: `fixture-session-${i}`, ip: `203.0.113.${i + 1}`, ua: 'Mozilla/5.0 Chrome/128 (Macintosh)', created_at: 1757000000 - i * 60, current: i === 0,
      })) },
      '/api/me/logins': { data: empty ? [] : Array.from({ length: 20 }, (_, i) => ({
        ok: i % 3 !== 0, at: new Date(Date.UTC(2026, 8, 28, 12, i)).toISOString(),
        ip: `198.51.100.${i + 1}`, ua: 'Mozilla/5.0 Chrome/128 (Macintosh)', reason: i % 3 === 0 ? 'invalid_credentials' : null,
      })) },
    }
    return route.fulfill({ json: responses[path] ?? { data: [] } })
  })
}

async function expectWithinViewport(page: Page) {
  expect(await page.evaluate(() => ({
    horizontal: document.documentElement.scrollWidth > innerWidth + 1,
    vertical: document.documentElement.scrollHeight > innerHeight + 1,
  }))).toEqual({ horizontal: false, vertical: false })
  const left = (await page.locator('[data-slot="security-totp"]').boundingBox())!
  const right = (await page.locator('[data-slot="security-sidebar"]').boundingBox())!
  expect(Math.abs(left.y - right.y)).toBeLessThanOrEqual(1)
  expect(Math.abs(left.height - right.height)).toBeLessThanOrEqual(1)
  const height = page.viewportSize()!.height
  expect(right.y + right.height).toBeLessThanOrEqual(height - 19)
  expect(right.y + right.height).toBeGreaterThanOrEqual(height - 21)
}

for (const { width, height, language, devices } of [
  { width: 1920, height: 1080, language: 'zh-CN', devices: 1 },
  { width: 1440, height: 900, language: 'zh-CN', devices: 10 },
  { width: 1024, height: 768, language: 'zh-CN', devices: 10 },
  { width: 1280, height: 720, language: 'en', devices: 10 },
]) {
  test(`安全页高度 ${width}x${height} ${language}：两栏对齐，登录与会话仅保留三条摘要`, async ({ page }) => {
    await prepare(page, { language, devices })
    await page.setViewportSize({ width, height })
    await page.goto('/portal/security')
    const logins = page.locator('[data-slot="security-logins"]')
    const sessions = page.locator('[data-slot="security-sessions"]')
    await expect(logins.getByRole('listitem')).toHaveCount(3)
    await expect(sessions.getByRole('listitem')).toHaveCount(Math.min(devices, 3))
    await expectWithinViewport(page)
    const bounds = (await logins.boundingBox())!
    const right = (await page.locator('[data-slot="security-sidebar"]').boundingBox())!
    expect(Math.abs(bounds.y + bounds.height - right.y - right.height)).toBeLessThanOrEqual(1)
    await expect(sessions.getByRole('button')).toHaveCount(0)
    await expect(logins.getByRole('button')).toHaveCount(0)
    const sessionsLink = sessions.getByRole('link', { name: language === 'en' ? 'View all sessions' : '查看全部会话', exact: true })
    await sessionsLink.scrollIntoViewIfNeeded()
    await expect(sessionsLink).toBeInViewport({ ratio: 1 })
    await expect(sessionsLink).toHaveAttribute('href', '/portal/profile?tab=signins#profile-sessions')
    const loginsLink = logins.getByRole('link', { name: language === 'en' ? 'View all sign-in records' : '查看全部登录记录', exact: true })
    await loginsLink.scrollIntoViewIfNeeded()
    await expect(loginsLink).toBeInViewport({ ratio: 1 })
    await expect(loginsLink).toHaveAttribute('href', '/portal/profile?tab=signins#profile-logins')
    await expectWithinViewport(page)
    await logins.focus()
    await logins.press('Home')
    await logins.evaluate((node) => { node.scrollTop = 0 })
    await sessions.evaluate((node) => { node.scrollTop = 0 })
    await page.screenshot({ path: `test-results/security-fit-${width}-${language}.png`, animations: 'disabled' })
  })
}

test('低矮窗口与公告：仅内容区内部滚动，底部记录和操作仍可到达', async ({ page }) => {
  await prepare(page, { notice: true })
  await page.setViewportSize({ width: 1280, height: 600 })
  await page.goto('/portal/security')
  await expect(page.getByText('Test notice')).toBeVisible()
  await expect(page.locator('[data-slot="security-logins"] li')).toHaveCount(3)
  await expectWithinViewport(page)
  const side = page.locator('[data-slot="security-sidebar"]')
  expect(await side.evaluate((node) => node.scrollHeight > node.clientHeight)).toBe(true)
  const sessions = side.locator('[data-slot="security-sessions"]')
  expect((await sessions.boundingBox())!.height).toBeGreaterThanOrEqual(160)
  const sessionsLink = sessions.getByRole('link', { name: '查看全部会话', exact: true })
  await sessionsLink.scrollIntoViewIfNeeded()
  await expect(sessionsLink).toBeInViewport({ ratio: 1 })
  const loginsLink = side.getByRole('link', { name: '查看全部登录记录', exact: true })
  await loginsLink.scrollIntoViewIfNeeded()
  await expect(loginsLink).toBeInViewport({ ratio: 1 })
  await expectWithinViewport(page)
})

test('空记录仍填满右栏，手机单列正常滚动而不裁掉内容', async ({ page }) => {
  await prepare(page, { empty: true })
  await page.setViewportSize({ width: 1440, height: 900 })
  await page.goto('/portal/security')
  await expect(page.getByText('还没有登录记录（API Key 登录不计入）。')).toBeVisible()
  await expectWithinViewport(page)
  await page.setViewportSize({ width: 390, height: 844 })
  const recent = page.locator('[data-slot="security-logins"]')
  await recent.scrollIntoViewIfNeeded()
  await expect(recent).toBeInViewport({ ratio: 1 })
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
})

test('进入安全页后密码与导航搜索属于独立表单，密码输入不触发搜索，手动搜索仍可跳转', async ({ page }) => {
  await prepare(page)
  await page.goto('/portal/ledger')
  const sidebar = page.locator('#app-navigation')
  const search = sidebar.getByRole('searchbox', { name: '搜索功能', exact: true })
  await sidebar.getByRole('link', { name: '安全', exact: true }).click()
  await expect(page).toHaveURL(/\/portal\/security$/)
  await expect(search).toHaveValue('')

  const password = page.locator('#totp-password')
  // 密码管理器按原生表单归属识别账号字段；不能再把侧栏搜索与密码配成一组。
  expect(await password.evaluate((input: HTMLInputElement) => {
    const search = document.querySelector<HTMLInputElement>('#app-navigation input[type="search"]')!
    return {
      isolated: input.form !== null && search.form !== null && input.form !== search.form,
      credentials: input.form ? Array.from(new FormData(input.form).keys()) : [],
      searchFields: search.form ? Array.from(new FormData(search.form).keys()) : [],
    }
  })).toEqual({ isolated: true, credentials: ['password'], searchFields: ['navigation-query'] })
  await expect(search).toHaveAttribute('autocomplete', 'off')
  await expect(password).toHaveAttribute('autocomplete', 'current-password')
  await password.fill('fixture-security-password')
  await password.press('Enter')
  await expect(page).toHaveURL(/\/portal\/security$/)
  await expect(page.getByRole('button', { name: '开始绑定', exact: true })).toBeEnabled()
  await expect(search).toHaveValue('')
  await expect(sidebar.getByRole('link', { name: '安全', exact: true })).toBeVisible()

  await search.fill('安全')
  await expect(sidebar.getByRole('navigation').getByRole('link')).toHaveCount(1)
  await search.press('Enter')
  await expect(page).toHaveURL(/\/portal\/security$/)
  await expect(search).toHaveValue('')
  await expect(sidebar.getByRole('link', { name: '个人中心', exact: true })).toBeVisible()

  const enrollments: unknown[] = []
  await page.route('**/auth/totp/enroll', (route) => {
    expect(route.request().method()).toBe('POST')
    enrollments.push(route.request().postDataJSON())
    return route.fulfill({ json: { otpauth_url: 'otpauth://totp/Okapi:fixture?secret=JBSWY3DPEHPK3PXP', pending: 'fixture-pending' } })
  })
  await page.getByRole('button', { name: '开始绑定', exact: true }).click()
  await expect(page.locator('#code')).toBeVisible()
  expect(enrollments).toEqual([{ password: 'fixture-security-password' }])
  await expect(search).toHaveValue('')
})
