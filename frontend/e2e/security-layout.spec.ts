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
  test(`安全页高度 ${width}x${height} ${language}：两栏铺满对齐，展开记录只在卡片内滚动`, async ({ page }) => {
    await prepare(page, { language, devices })
    await page.setViewportSize({ width, height })
    await page.goto('/portal/security')
    const logins = page.locator('[data-slot="security-logins"]')
    const sessions = page.locator('[data-slot="security-sessions"]')
    await expect(logins.getByRole('listitem')).toHaveCount(8)
    await expect(sessions.getByRole('listitem')).toHaveCount(devices)
    await expectWithinViewport(page)
    const before = (await logins.boundingBox())!
    const right = (await page.locator('[data-slot="security-sidebar"]').boundingBox())!
    expect(Math.abs(before.y + before.height - right.y - right.height)).toBeLessThanOrEqual(1)
    await logins.getByRole('button', { name: language === 'en' ? 'Show 12 more' : '展开其余 12 条', exact: true }).click()
    await expect(logins.getByRole('listitem')).toHaveCount(20)
    expect((await logins.boundingBox())!.height).toBeCloseTo(before.height, 1)
    expect(await logins.evaluate((node) => node.scrollHeight > node.clientHeight)).toBe(true)
    await logins.getByRole('listitem').last().scrollIntoViewIfNeeded()
    await expect(logins.getByRole('listitem').last()).toBeInViewport()
    const revokeAll = sessions.getByRole('button', { name: language === 'en' ? 'Revoke all sessions' : '吊销全部会话', exact: true })
    await revokeAll.scrollIntoViewIfNeeded()
    await expect(revokeAll).toBeInViewport({ ratio: 1 })
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
  await expect(page.locator('[data-slot="security-logins"] li')).toHaveCount(8)
  await expectWithinViewport(page)
  const side = page.locator('[data-slot="security-sidebar"]')
  expect(await side.evaluate((node) => node.scrollHeight > node.clientHeight)).toBe(true)
  const sessions = side.locator('[data-slot="security-sessions"]')
  expect((await sessions.boundingBox())!.height).toBeGreaterThanOrEqual(160)
  const revokeAll = sessions.getByRole('button', { name: '吊销全部会话', exact: true })
  await revokeAll.scrollIntoViewIfNeeded()
  await expect(revokeAll).toBeInViewport({ ratio: 1 })
  const expand = side.getByRole('button', { name: '展开其余 12 条', exact: true })
  await expand.click()
  const last = side.locator('[data-slot="security-logins"] li').last()
  await last.scrollIntoViewIfNeeded()
  await expect(last).toBeInViewport()
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
