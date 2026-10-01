import { expect, test } from '@playwright/test'
import type { Page } from '@playwright/test'
import { fileURLToPath } from 'node:url'

async function prepare(page: Page) {
  await page.addInitScript(() => {
    localStorage.setItem('okapi.key', 'profile-signins-fixture')
    localStorage.setItem('okapi.login-mode', 'account')
    localStorage.setItem('okapi.lang', 'zh-CN')
  })
  const logins = Array.from({ length: 20 }, (_, i) => ({
    ok: i % 3 !== 0, at: new Date(Date.UTC(2026, 8, 30, 12, 20 - i)).toISOString(),
    ip: `198.51.100.${i + 1}`, ua: 'Mozilla/5.0 Chrome/128 (Macintosh)', reason: i % 3 === 0 ? 'invalid_credentials' : null,
  }))
  const initialSessions = Array.from({ length: 10 }, (_, i) => ({
    sid: (i + 1).toString(16).padStart(32, '0'),
    ip: `203.0.113.${i >= 1 && i <= 3 ? 2 : i + 1}`,
    ua: i === 3 ? 'Mozilla/5.0 Firefox/128 (Macintosh)' : 'Mozilla/5.0 Chrome/128 (Macintosh)',
    created_at: 1757000000 - i * 60, current: i === 0,
  }))
  let sessions = [...initialSessions]
  const writes: string[] = [], requests: string[] = []
  await page.route('**/*', (route) => {
    const request = route.request(), path = new URL(request.url()).pathname
    if (request.isNavigationRequest()) return route.fulfill({ path: fileURLToPath(new URL('../dist/index.html', import.meta.url)), contentType: 'text/html' })
    if (!/^\/(api|auth|admin)\//.test(path)) return route.continue()
    requests.push(path)
    if (request.method() === 'DELETE' && path.startsWith('/api/me/sessions')) {
      writes.push(path)
      sessions = path === '/api/me/sessions' ? [] : sessions.filter((session) => path !== `/api/me/sessions/${session.sid}`)
      return route.fulfill({ json: { ok: true } })
    }
    expect(request.method(), '除此以外不得提交写请求').toBe('GET')
    const responses: Record<string, unknown> = {
      '/api/me': { user_id: 1, key_id: 1, role: 1, permissions: [], group: 'default', balance_micro: 1000000, has_web_session: true },
      '/api/me/profile': { username: 'Alice', email: 'alice@example.test', language: 'zh-CN', created_at: '2026-01-01T00:00:00Z' },
      '/api/me/sessions': { data: sessions, limit: 10 },
      '/api/me/logins': { data: logins },
      '/api/me/stats/activity': { year: 2026, scope: 'user', today: '2026-09-30', timezone: 'UTC', first_year: 2026, data: [] },
      '/api/notice': { notice: null },
    }
    return route.fulfill({ json: responses[path] ?? { data: [] } })
  })
  return { requests, writes, initialSessions }
}

for (const width of [390, 1440]) {
  test(`登录与会话 ${width}px：安全页三条摘要，两个入口跳转个人中心对应完整列表`, async ({ page }) => {
    const { requests, writes } = await prepare(page)
    await page.setViewportSize({ width, height: 900 })
    await page.goto('/portal/security')
    const previewSessions = page.locator('[data-slot="security-sessions"]')
    const previewLogins = page.locator('[data-slot="security-logins"]')
    await expect(previewLogins.getByRole('listitem')).toHaveCount(3)
    await expect(previewSessions.getByRole('listitem')).toHaveCount(3)
    await expect(previewSessions.getByRole('button')).toHaveCount(0)
    await expect(previewLogins.getByRole('button')).toHaveCount(0)
    await previewLogins.getByRole('link', { name: '查看全部登录记录' }).click()
    await expect(page).toHaveURL(/\/portal\/profile\?tab=signins#profile-logins$/)
    await expect(page.getByRole('tab', { name: '登录与会话' })).toHaveAttribute('aria-selected', 'true')
    await expect(page.locator('#profile-logins').getByRole('listitem')).toHaveCount(5)
    await expect(page.locator('#profile-logins').getByText('1–5 / 共 20', { exact: true })).toBeVisible()
    await expect(page.locator('#profile-sessions').getByRole('listitem')).toHaveCount(5)
    await expect(page.locator('#profile-sessions').getByText('1–5 / 共 9', { exact: true })).toBeVisible()
    await page.locator('#profile-logins').getByRole('button', { name: '下一页', exact: true }).click()
    await expect(page.locator('#profile-logins').getByText('6–10 / 共 20', { exact: true })).toBeVisible()
    await page.locator('#profile-sessions').getByRole('button', { name: '下一页', exact: true }).click()
    await expect(page.locator('#profile-sessions').getByRole('listitem')).toHaveCount(4)
    await expect(page.locator('#profile-sessions').getByText('6–9 / 共 9', { exact: true })).toBeVisible()
    expect(requests).not.toContain('/api/me/stats/activity')
    await page.reload()
    await expect(page.getByRole('tab', { name: '登录与会话' })).toHaveAttribute('aria-selected', 'true')
    await expect(page.locator('#profile-logins').getByRole('listitem')).toHaveCount(5)
    await page.getByRole('main').getByRole('link', { name: '安全', exact: true }).click()
    await previewSessions.getByRole('link', { name: '查看全部会话' }).click()
    await expect(page).toHaveURL(/\/portal\/profile\?tab=signins#profile-sessions$/)
    await expect(page.locator('#profile-sessions').getByRole('heading', { name: '有效登录会话' })).toBeInViewport()
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    expect(writes).toHaveLength(0)
    await page.evaluate(() => window.scrollTo(0, 0))
    await page.screenshot({ path: `test-results/profile-signins-${width}.png`, fullPage: true, animations: 'disabled' })
  })
}

test('个人中心会话吊销：同一浏览器多会话一起吊销，同 IP 的其他浏览器保留，全部吊销后刷新为空', async ({ page }) => {
  const { writes, initialSessions } = await prepare(page)
  await page.goto('/portal/profile?tab=signins')
  const sessions = page.locator('#profile-sessions')
  await expect(sessions.getByRole('listitem')).toHaveCount(5)
  await expect(sessions.getByText('1–5 / 共 9', { exact: true })).toBeVisible()
  await expect(sessions.getByText('当前浏览器')).toBeVisible()
  const grouped = sessions.getByRole('listitem').filter({ hasText: '2 个会话' })
  await grouped.getByRole('button', { name: '吊销', exact: true }).click()
  await expect(sessions.getByText('1–5 / 共 8', { exact: true })).toBeVisible()
  expect(writes).toEqual(initialSessions.slice(1, 3).map((session) => `/api/me/sessions/${session.sid}`))
  await expect(sessions.getByText('Mozilla/5.0 Firefox/128 (Macintosh)', { exact: true })).toBeVisible()
  await sessions.getByRole('button', { name: '吊销全部会话', exact: true }).click()
  await expect(sessions.getByRole('listitem')).toHaveCount(0)
  expect(writes.at(-1)).toBe('/api/me/sessions')
  await expect(page.locator('#profile-logins').getByRole('listitem')).toHaveCount(5)
  await page.getByRole('main').getByRole('link', { name: '安全', exact: true }).click()
  await expect(page.locator('[data-slot="security-sessions"]').getByRole('listitem')).toHaveCount(0)
})

test('登录列表接口失败显示错误，可分别重试，不显示为没有登录或会话', async ({ page }) => {
  await prepare(page)
  for (const path of ['/api/me/logins', '/api/me/sessions']) {
    await page.route(`**${path}`, (route) => route.fulfill({ status: 500, json: { error: { code: 'internal_error' } } }))
  }
  await page.goto('/portal/profile?tab=signins')
  const logins = page.locator('#profile-logins'), sessions = page.locator('#profile-sessions')
  await expect(logins.getByRole('alert')).toContainText('服务内部错误')
  await expect(sessions.getByRole('alert')).toContainText('服务内部错误')
  await expect(logins.getByText('还没有登录记录（API Key 登录不计入）。')).toHaveCount(0)
  await expect(sessions.getByText('没有有效的 web 会话。', { exact: false })).toHaveCount(0)
  await page.unroute('**/api/me/logins')
  await page.unroute('**/api/me/sessions')
  await logins.getByRole('button', { name: '重试' }).click()
  await sessions.getByRole('button', { name: '重试' }).click()
  await expect(logins.getByRole('listitem')).toHaveCount(5)
  await expect(sessions.getByRole('listitem')).toHaveCount(5)
})

test('登录和会话独立分页：筛选重置页码，切换每页条数与页签保留各自状态', async ({ page }) => {
  await prepare(page)
  await page.goto('/portal/profile?tab=signins')
  const logins = page.locator('#profile-logins'), sessions = page.locator('#profile-sessions')
  await logins.getByRole('button', { name: '4', exact: true }).click()
  await expect(logins.getByText('16–20 / 共 20', { exact: true })).toBeVisible()
  await expect(logins.getByText('198.51.100.16', { exact: true })).toBeVisible()
  await expect(logins.getByText('198.51.100.1', { exact: true })).toHaveCount(0)
  await expect(sessions.getByText('1–5 / 共 9', { exact: true })).toBeVisible()
  await sessions.getByRole('button', { name: '下一页', exact: true }).click()
  await expect(sessions.getByRole('listitem')).toHaveCount(4)
  await expect(sessions.getByText('6–9 / 共 9', { exact: true })).toBeVisible()
  await expect(logins.getByText('16–20 / 共 20', { exact: true })).toBeVisible()
  await logins.getByRole('button', { name: '仅看失败（7）', exact: true }).click()
  await expect(logins.getByText('1–5 / 共 7', { exact: true })).toBeVisible()
  await logins.getByRole('button', { name: '下一页', exact: true }).click()
  await expect(logins.getByRole('listitem')).toHaveCount(2)
  await expect(logins.getByText('6–7 / 共 7', { exact: true })).toBeVisible()
  await expect(logins.getByText('成功', { exact: true })).toHaveCount(0)
  await logins.getByLabel('每页条数', { exact: true }).selectOption('10')
  await expect(logins.getByRole('listitem')).toHaveCount(7)
  await expect(logins.getByText('1–7 / 共 7', { exact: true })).toBeVisible()
  await page.getByRole('tab', { name: '基本信息', exact: true }).click()
  await page.getByRole('tab', { name: '登录与会话', exact: true }).click()
  await expect(logins.getByLabel('每页条数', { exact: true })).toHaveValue('10')
  await expect(sessions.getByText('6–9 / 共 9', { exact: true })).toBeVisible()
  await logins.getByRole('button', { name: '仅看失败（7）', exact: true }).click()
  await expect(logins.getByRole('listitem')).toHaveCount(10)
  await expect(logins.getByText('1–10 / 共 20', { exact: true })).toBeVisible()
})

test('会话末页逐条吊销后自动返回有效页，全部吊销后移除分页', async ({ page }) => {
  const { writes } = await prepare(page)
  await page.goto('/portal/profile?tab=signins')
  const sessions = page.locator('#profile-sessions')
  await sessions.getByRole('button', { name: '下一页', exact: true }).click()
  await expect(sessions.getByText('6–9 / 共 9', { exact: true })).toBeVisible()
  for (const remaining of [3, 2, 1, 5]) {
    await sessions.getByRole('button', { name: '吊销', exact: true }).first().click()
    await expect(sessions.getByRole('listitem')).toHaveCount(remaining)
  }
  await expect(sessions.getByText('1–5 / 共 5', { exact: true })).toBeVisible()
  await expect(sessions.getByText('当前浏览器', { exact: true })).toBeVisible()
  expect(writes).toHaveLength(4)
  await sessions.getByRole('button', { name: '吊销全部会话', exact: true }).click()
  await expect(sessions.getByRole('listitem')).toHaveCount(0)
  await expect(sessions.getByRole('navigation', { name: '分页', exact: true })).toHaveCount(0)
})
