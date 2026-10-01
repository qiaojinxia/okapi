import { test, expect } from '@playwright/test'
import type { Page } from '@playwright/test'
import { fileURLToPath } from 'node:url'

async function prepare(page: Page, session = true) {
  await page.addInitScript(() => {
    localStorage.setItem('okapi.key', 'profile-fixture')
    localStorage.setItem('okapi.login-mode', 'account')
    localStorage.setItem('okapi.lang', 'zh-CN')
  })
  let profile = { username: 'Alice', email: 'alice@example.test', language: 'zh-CN', created_at: '2026-01-01T00:00:00Z' }
  const writes: unknown[] = [], requests: string[] = []
  await page.route('**/*', (route) => {
    const request = route.request(), url = new URL(request.url())
    if (request.isNavigationRequest()) return route.fulfill({ path: fileURLToPath(new URL('../dist/index.html', import.meta.url)), contentType: 'text/html' })
    if (!/^\/(api|admin|auth)\//.test(url.pathname)) return route.continue()
    requests.push(url.pathname)
    if (url.pathname === '/api/me/profile') {
      if (!session) return route.fulfill({ status: 401, json: { error: { code: 'invalid_api_key' } } })
      if (request.method() === 'PATCH') {
        const body = request.postDataJSON()
        writes.push(body)
        if (body.username === 'taken') return route.fulfill({ status: 409, json: { error: { code: 'profile_username_taken' } } })
        profile = { ...profile, ...body }
      }
      return route.fulfill({ json: profile })
    }
    const responses: Record<string, unknown> = {
      '/api/me': { user_id: 1, key_id: 1, role: 1, group: 'default', balance_micro: 1000000, permissions: [], has_web_session: session },
      '/api/me/stats/activity': { year: 2026, scope: 'user', today: '2026-09-30', timezone: 'UTC', first_year: 2026, data: [] },
    }
    return route.fulfill({ json: responses[url.pathname] ?? { data: [] } })
  })
  return { writes, requests }
}

for (const width of [390, 1440]) {
  test(`个人基本信息 ${width}px：保存真实资料，页签保留草稿和链接，刷新读取已保存值`, async ({ page }) => {
    const { writes, requests } = await prepare(page)
    await page.setViewportSize({ width, height: 900 })
    await page.goto('/portal/profile?tab=info')
    await expect(page.getByRole('tab', { name: '基本信息' })).toHaveAttribute('aria-selected', 'true')
    const username = page.getByLabel('用户名', { exact: true })
    await expect(username).toHaveValue('Alice')
    expect(requests).not.toContain('/api/me/stats/activity')
    expect(requests).not.toContain('/api/me/logins')
    expect(requests).not.toContain('/api/me/sessions')
    await expect(page.getByRole('button', { name: '保存', exact: true })).toBeDisabled()
    await username.fill('  新名字  ')
    await page.getByRole('tab', { name: '登录与会话' }).click()
    await page.getByRole('tab', { name: '基本信息' }).click()
    await expect(username).toHaveValue('  新名字  ')
    await page.getByRole('tab', { name: '历史用量' }).click()
    await page.getByRole('tab', { name: '基本信息' }).click()
    await expect(username).toHaveValue('  新名字  ')
    await page.getByRole('button', { name: '保存', exact: true }).click()
    await expect.poll(() => writes).toEqual([{ username: '新名字', language: 'zh-CN' }])
    await expect(page.getByRole('button', { name: '保存', exact: true })).toBeDisabled()
    await page.reload()
    await expect(username).toHaveValue('新名字')
    await expect(page.getByText('alice@example.test', { exact: true })).toBeVisible()
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    await page.screenshot({ path: `test-results/profile-info-${width}.png`, fullPage: true, animations: 'disabled' })
  })
}

test('个人基本信息：空用户名不提交，重名保留输入，撤销恢复，语言偏好保存并生效', async ({ page }) => {
  const { writes } = await prepare(page)
  await page.goto('/portal/profile?tab=info')
  const username = page.getByLabel('用户名', { exact: true })
  await username.fill(' ')
  await expect(page.getByRole('button', { name: '保存', exact: true })).toBeDisabled()
  expect(writes).toHaveLength(0)
  await username.fill('taken')
  await page.getByRole('button', { name: '保存', exact: true }).click()
  await expect(page.getByRole('alert')).toContainText('该用户名已被使用')
  await expect(username).toHaveValue('taken')
  await page.getByRole('button', { name: '撤销修改' }).click()
  await expect(username).toHaveValue('Alice')
  await page.getByLabel('语言偏好', { exact: true }).selectOption('en')
  await page.getByRole('button', { name: '保存', exact: true }).click()
  await expect(page.getByRole('tab', { name: 'Basic information' })).toBeVisible()
  expect(writes.at(-1)).toEqual({ username: 'Alice', language: 'en' })
  expect(await page.evaluate(() => localStorage.getItem('okapi.lang'))).toBe('en')
})

test('API Key 登录的基本信息提示账户登录，不展示可编辑表单', async ({ page }) => {
  const { writes } = await prepare(page, false)
  await page.goto('/portal/profile?tab=info')
  await expect(page.getByRole('status')).toContainText('请使用邮箱密码或第三方账户登录')
  await expect(page.getByLabel('用户名', { exact: true })).toHaveCount(0)
  await expect(page.getByRole('link', { name: '邮箱登录', exact: true })).toHaveAttribute('href', '/')
  expect(writes).toHaveLength(0)
  await page.getByRole('link', { name: '邮箱登录', exact: true }).click()
  await expect(page).toHaveURL(/\/$/)
  expect(await page.evaluate(() => localStorage.getItem('okapi.key'))).toBeNull()
})
