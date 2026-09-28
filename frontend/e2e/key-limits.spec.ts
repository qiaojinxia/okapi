import { expect, test } from '@playwright/test'
import type { Page } from '@playwright/test'
import { fileURLToPath } from 'node:url'

async function prepare(page: Page, supported = true, quota = 10000000) {
  const writes: { method: string; body: Record<string, unknown> }[] = []
  await page.addInitScript(() => {
    localStorage.setItem('okapi.key', 'key-limits-fixture')
    localStorage.setItem('okapi.login-mode', 'account')
    localStorage.setItem('okapi.lang', 'zh-CN')
  })
  await page.route('**/*', (route) => {
    const request = route.request(), path = new URL(request.url()).pathname
    if (request.isNavigationRequest()) return route.fulfill({ path: fileURLToPath(new URL('../dist/index.html', import.meta.url)), contentType: 'text/html' })
    if (!/^\/(api|admin|auth)\//.test(path)) return route.continue()
    if (request.method() !== 'GET') {
      writes.push({ method: request.method(), body: request.postDataJSON() })
      return route.fulfill({ json: { ok: true, key_id: 42, api_key: 'sk-fixture-created', copy_available: false } })
    }
    const responses: Record<string, unknown> = {
      '/api/me': { user_id: 7, key_id: 1, has_web_session: true, role: 1, permissions: [], balance_micro: 20000000, group: 'default' },
      '/api/notice': { notice: null },
      '/api/me/groups': { current: 'default', data: [{ code: 'default', ratio: '1', source: 'default' }, { code: 'vip', ratio: '0.8', source: 'assigned' }] },
      '/api/pricing': { groups: [{ code: 'default', is_default: true }, { code: 'vip' }], models: [{ model: 'model-a', display_name: 'Model A', groups: ['default'] }, { model: 'model-b', groups: ['vip'] }] },
      '/api/me/keys': { key_limits_supported: supported, total: 1, data: [{ id: 42, name: 'production', key_prefix: 'sk-fixture', copy_status: 'not_saved', status: 1,
        used_micro: 2000000, amount_micro: 100, requests: 3, quota_mode: 1, quota_micro: quota,
        expires_at: '2030-10-01T12:00:00Z', model_allowlist: ['model-a'], rpm_limit: null,
        created_at: '2026-09-28T00:00:00Z', group_override: null, ip_allowlist: null }] },
    }
    return route.fulfill({ json: responses[path] ?? { data: [] } })
  })
  await page.goto('/portal/keys')
  return writes
}

test('密钥限制：创建、模型建议、精确金额和本地过期时间', async ({ page }) => {
  const writes = await prepare(page)
  await page.getByRole('button', { name: '新建密钥', exact: true }).click()
  const dialog = page.getByRole('dialog', { name: '新建密钥' })
  await dialog.getByLabel('名称', { exact: true }).fill('ci')
  await expect(dialog.getByLabel('分组', { exact: true })).toBeVisible()
  await dialog.getByLabel('累计消费上限（USD）').fill('12.345678')
  await dialog.getByLabel('过期时间', { exact: true }).fill('2030-10-01T12:34:56')
  const models = dialog.getByLabel('允许访问的模型', { exact: true })
  await models.fill('model')
  await expect(page.getByRole('option').filter({ hasText: 'model-a' })).toBeVisible()
  await expect(page.getByRole('option').filter({ hasText: 'model-b' })).toHaveCount(0)
  await page.getByRole('option').filter({ hasText: 'model-a' }).click()
  await page.screenshot({ path: 'test-results/key-limits-editor.png', animations: 'disabled' })
  await dialog.getByRole('button', { name: '新建', exact: true }).click()
  await expect.poll(() => writes.length).toBe(1)
  const expectedDate = await page.evaluate(() => new Date('2030-10-01T12:34:56').toISOString())
  expect(writes[0]).toEqual({ method: 'POST', body: { name: 'ci', quota_micro: 12345678, expires_at: expectedDate, model_allowlist: ['model-a'] } })
})

test('密钥限制：编辑回显、权威消费、清空解除三项限制', async ({ page }) => {
  const writes = await prepare(page)
  const row = page.locator('tbody tr')
  await expect(row).toContainText('限定 1 个模型')
  await expect(row).toContainText('US$2.00')
  await expect(row).toContainText('上限 US$10.00')
  await row.getByRole('button', { name: '编辑', exact: true }).click()
  const dialog = page.getByRole('dialog', { name: '编辑密钥' })
  await expect(dialog.getByLabel('累计消费上限（USD）')).toHaveValue('10')
  await expect(dialog.locator('#key-expires')).not.toHaveValue('')
  await dialog.getByRole('button', { name: '永不过期', exact: true }).click()
  await dialog.getByLabel('累计消费上限（USD）').fill('')
  await dialog.getByLabel('允许访问的模型', { exact: true }).focus()
  await page.keyboard.press('Backspace')
  await dialog.getByRole('button', { name: '保存', exact: true }).click()
  await expect.poll(() => writes.length).toBe(1)
  expect(writes[0].body).toEqual({ name: 'production', group_code: null, ip_allowlist: null, expires_at: null, quota_micro: null, model_allowlist: null })
})

test('密钥限制：非法金额和过去时间禁止提交，留空不限制', async ({ page }) => {
  const writes = await prepare(page)
  await page.getByRole('button', { name: '新建密钥', exact: true }).click()
  const dialog = page.getByRole('dialog')
  await dialog.getByLabel('名称', { exact: true }).fill('unlimited')
  const quota = dialog.getByLabel('累计消费上限（USD）')
  const save = dialog.getByRole('button', { name: '新建', exact: true })
  for (const invalid of ['0', '-1', '1.0000001', 'no', '9007199255']) {
    await quota.fill(invalid)
    await expect(save).toBeDisabled()
    await expect(dialog.getByRole('alert')).toBeVisible()
  }
  await quota.fill('0.000001')
  await expect(save).toBeEnabled()
  await quota.fill('')
  await dialog.getByLabel('过期时间', { exact: true }).fill('2020-01-01T12:00')
  await expect(save).toBeDisabled()
  await dialog.getByRole('button', { name: '永不过期', exact: true }).click()
  await save.click()
  await expect.poll(() => writes.length).toBe(1)
  expect(writes[0].body).toEqual({ name: 'unlimited', quota_micro: null })
})

test('密钥限制：旧后端不可静默忽略额度配置', async ({ page }) => {
  await prepare(page, false)
  await page.getByRole('button', { name: '新建密钥', exact: true }).click()
  await expect(page.getByLabel('累计消费上限（USD）')).toBeDisabled()
  await expect(page.getByText('当前后端尚未支持额度限制，升级后可配置。')).toBeVisible()
})

test('密钥限制：最大金额编辑回显不丢失微美元精度', async ({ page }) => {
  const writes = await prepare(page, true, 9007199254740991)
  await page.locator('tbody tr').getByRole('button', { name: '编辑', exact: true }).click()
  const dialog = page.getByRole('dialog')
  await expect(dialog.getByLabel('累计消费上限（USD）')).toHaveValue('9007199254.740991')
  await dialog.getByRole('button', { name: '保存', exact: true }).click()
  await expect.poll(() => writes.length).toBe(1)
  expect(writes[0].body.quota_micro).toBe(9007199254740991)
})
