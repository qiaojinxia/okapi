import { expect, test } from '@playwright/test'
import type { Page } from '@playwright/test'
import { fileURLToPath } from 'node:url'

const token = 'sk-okapi-copy-fixture-not-a-real-secret-12345678901234'
async function prepare(page: Page, { session = true, status = 'available' } = {}) {
  const copies: { path: string; method: string; body: unknown }[] = []
  await page.addInitScript(() => {
    localStorage.setItem('okapi.key', 'copy-auth-fixture')
    localStorage.setItem('okapi.login-mode', 'account')
    localStorage.setItem('okapi.lang', 'zh-CN')
    localStorage.setItem('okapi.theme', 'light')
  })
  await page.route('**/*', (route) => {
    const request = route.request(), path = new URL(request.url()).pathname
    if (request.isNavigationRequest()) return route.fulfill({ path: fileURLToPath(new URL('../dist/index.html', import.meta.url)), contentType: 'text/html' })
    if (!/^\/(api|admin|auth)\//.test(path)) return route.continue()
    if (path === '/auth/keys/42/copy') {
      copies.push({ path, method: request.method(), body: request.postDataJSON() })
      return route.fulfill({ json: { api_key: token } })
    }
    const responses: Record<string, unknown> = {
      '/api/me': { user_id: 7, key_id: 1, has_web_session: session, role: 1, permissions: [], balance_micro: 20000000, group: 'default' },
      '/api/notice': { notice: null }, '/api/pricing': { models: [], groups: [] },
      '/api/me/groups': { data: [] },
      '/api/me/keys': { total: 1, data: [{ id: 42, name: 'production', key_prefix: 'sk-okapi-copy-fix', copy_status: status,
        status: 1, used_micro: 100, amount_micro: 100, requests: 1, rpm_limit: null,
        created_at: '2026-09-28T00:00:00Z', group_override: null, ip_allowlist: null }] },
    }
    return route.fulfill({ json: responses[path] ?? { data: [] } })
  })
  return copies
}

const label = '复制 production（#42）的完整 Token'
test('密钥复制：Token 首列按钮紧邻脱敏值、按需获取完整值，刷新仍可复制且不持久化明文', async ({ page, context }) => {
  await context.grantPermissions(['clipboard-read', 'clipboard-write'])
  const copies = await prepare(page)
  await page.setViewportSize({ width: 1440, height: 900 })
  await page.goto('/portal/keys')
  const button = page.getByRole('button', { name: label })
  await expect(button).toBeVisible()
  expect(copies).toHaveLength(0)
  await expect(page.getByRole('columnheader').first()).toHaveText('Token')
  const cell = page.locator('tbody td').first()
  await expect(cell).toContainText('sk-okapi-copy-fix…')
  await expect(cell.getByRole('button')).toHaveAccessibleName(label)
  expect((await page.locator('tbody tr').boundingBox())!.height).toBeCloseTo(56, 2)
  await button.focus()
  await page.keyboard.press('Enter')
  await expect(page.getByRole('button', { name: '已复制到剪贴板', exact: true })).toBeVisible()
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(token)
  expect(copies).toEqual([{ path: '/auth/keys/42/copy', method: 'POST', body: {} }])
  await expect(page.locator('body')).not.toContainText(token)
  expect(await page.evaluate(() => JSON.stringify([localStorage, sessionStorage]))).not.toContain(token)
  await page.screenshot({ path: 'test-results/key-copy-button.png', animations: 'disabled' })
  await page.reload()
  await page.getByRole('button', { name: label }).click()
  await expect.poll(() => copies.length).toBe(2)
  await expect(page.getByRole('button', { name: '已复制到剪贴板', exact: true })).toBeVisible()
})

for (const variant of [
  { status: 'not_saved', session: true, message: '这把密钥未保存加密副本' },
  { status: 'unavailable', session: true, message: '暂时无法复制' },
  { status: 'available', session: false, message: '复制完整 Token 需要有效的账号登录会话' },
]) test(`密钥复制：${variant.status} session=${variant.session} 清晰提示，不复制前缀也不发请求`, async ({ page }) => {
  const copies = await prepare(page, variant)
  await page.goto('/portal/keys')
  await page.getByRole('button', { name: label }).click()
  await expect(page.getByText(variant.message, { exact: false })).toBeVisible()
  expect(copies).toHaveLength(0)
})

test('密钥复制：会话过期与解密失败可重试，不显示成功', async ({ page }) => {
  await prepare(page)
  let status = 401, code = 'invalid_api_key'
  await page.route('**/auth/keys/42/copy', (route) => route.fulfill({ status, json: { error: { code } } }))
  await page.goto('/portal/keys')
  await page.getByRole('button', { name: label }).click()
  await expect(page.getByText(/复制完整 Token 需要有效的账号登录会话/)).toBeVisible()
  await expect(page.getByRole('button', { name: '已复制到剪贴板', exact: true })).toHaveCount(0)
  status = 503; code = 'key_copy_unavailable'
  await page.getByRole('button', { name: label }).click()
  await expect(page.getByText(/暂时无法复制/)).toBeVisible()
  await expect(page.getByRole('button', { name: label })).toBeEnabled()
})

test('密钥复制：剪贴板权限失败不误报成功，完整值不进入页面', async ({ page }) => {
  await prepare(page)
  await page.addInitScript(() => {
    Object.defineProperty(navigator, 'clipboard', { value: {
      write: () => Promise.reject(new DOMException('denied', 'NotAllowedError')),
      writeText: () => Promise.reject(new DOMException('denied', 'NotAllowedError')),
    } })
  })
  await page.goto('/portal/keys')
  await page.getByRole('button', { name: label }).click()
  await expect(page.getByText('复制失败，请检查浏览器剪贴板权限后重试。', { exact: true })).toBeVisible()
  await expect(page.getByRole('button', { name: '已复制到剪贴板', exact: true })).toHaveCount(0)
  await expect(page.locator('body')).not.toContainText(token)
})

test('密钥复制：加载中禁止重复请求，完成后恢复', async ({ page, context }) => {
  await context.grantPermissions(['clipboard-read', 'clipboard-write'])
  await prepare(page)
  let release!: () => void
  const gate = new Promise<void>((resolve) => { release = resolve })
  let count = 0
  await page.route('**/auth/keys/42/copy', async (route) => {
    count += 1
    await gate
    return route.fulfill({ json: { api_key: token } })
  })
  await page.goto('/portal/keys')
  await page.getByRole('button', { name: label }).click()
  await expect(page.getByRole('button', { name: label })).toBeDisabled()
  expect(count).toBe(1)
  release()
  await expect(page.getByRole('button', { name: '已复制到剪贴板', exact: true })).toBeEnabled()
  expect(count).toBe(1)
})
