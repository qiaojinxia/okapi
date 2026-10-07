import { expect, test } from '@playwright/test'
import type { Page } from '@playwright/test'
import { fileURLToPath } from 'node:url'

// 当前登录用的密钥：自己不能删除 / 停用（后端同样 409 current_key_in_use）；它在别处失效时自动回登录页。

const keyRow = (id: number, name: string) => ({
  id, name, key_prefix: `sk-okapi-${name}`, copy_status: 'available', status: 1, used_micro: 0, amount_micro: 0, requests: 0,
  rpm_limit: null, created_at: '2026-10-06T00:00:00Z', group_override: null, ip_allowlist: null,
})

async function prepare(page: Page, { revoked = false } = {}) {
  await page.addInitScript(() => {
    localStorage.setItem('okapi.key', 'current-key-fixture')
    localStorage.setItem('okapi.login-mode', 'account')
    localStorage.setItem('okapi.lang', 'zh-CN')
    localStorage.setItem('okapi.guide.7', 'dismissed')
  })
  await page.route('**/*', (route) => {
    const request = route.request(), path = new URL(request.url()).pathname
    if (request.isNavigationRequest()) return route.fulfill({ path: fileURLToPath(new URL('../dist/index.html', import.meta.url)), contentType: 'text/html' })
    if (!/^\/(api|admin|auth)\//.test(path)) return route.continue()
    // 当前登录用的 key 已在别处被删：所有带它的请求都是 401 invalid_api_key
    if (revoked && path.startsWith('/api/me')) {
      return route.fulfill({ status: 401, json: { error: { message: 'invalid_api_key', type: 'okapi_error', code: 'invalid_api_key' } } })
    }
    const responses: Record<string, unknown> = {
      '/api/me': { user_id: 7, key_id: 1, has_web_session: true, role: 1, permissions: [], balance_micro: 20000000, group: 'default' },
      '/api/notice': { notice: null }, '/api/pricing': { models: [], groups: [] },
      '/api/me/groups': { data: [] },
      '/api/me/keys': { total: 2, data: [keyRow(1, 'web'), keyRow(42, 'production')] },
    }
    return route.fulfill({ json: responses[path] ?? { data: [] } })
  })
}

test('当前登录的密钥标「当前登录」，停用与删除禁用并说明原因；其他密钥照常可操作', async ({ page }) => {
  await prepare(page)
  await page.goto('/portal/keys')
  const current = page.getByRole('row').filter({ hasText: 'web' })
  await expect(current.getByText('当前登录', { exact: true })).toBeVisible()
  await expect(current.getByRole('button', { name: '当前登录正在使用这把密钥，不能删除' })).toBeDisabled()
  await expect(current.getByRole('button', { name: '当前登录正在使用这把密钥，不能停用' })).toBeDisabled()
  const other = page.getByRole('row').filter({ hasText: 'production' })
  await expect(other.getByText('当前登录', { exact: true })).toHaveCount(0)
  await expect(other.getByRole('button', { name: '删除', exact: true })).toBeEnabled()
  await expect(other.getByRole('button', { name: '停用', exact: true })).toBeEnabled()
})

test('当前登录用的密钥在别处被删除：清掉本地 key 回登录页并提示，而不是每页报「API Key 无效」', async ({ page }) => {
  await prepare(page, { revoked: true })
  await page.goto('/portal/keys')
  await expect(page).toHaveURL(/\/$/)
  await expect(page.getByText('当前登录用的密钥已被删除或停用，请重新登录。')).toBeVisible()
  await expect.poll(() => page.evaluate(() => localStorage.getItem('okapi.key'))).toBeNull()
  await expect(page.getByRole('button', { name: '登录', exact: true })).toBeVisible()
})
