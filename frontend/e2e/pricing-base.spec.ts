import { expect, test } from '@playwright/test'
import { fileURLToPath } from 'node:url'
import { modelPrice } from '../src/features/public-pricing/catalog-data'
import type { PricingModel } from '../src/features/public-pricing/types'

test('目录价格使用发布基准，缓存及单位换算一致，按次价格不变', () => {
  const model = { mode: 'ratio', base_price_per_1m_micro: 3_000_000, model_ratio: '1.5', completion_ratio: '4', cache_ratio: '0.25', cache_write_ratio: '1.25' } as PricingModel
  expect(modelPrice(model, 'input', 0.8)).toBeCloseTo(3_600_000)
  expect(modelPrice(model, 'output', 0.8, '1K')).toBeCloseTo(14_400)
  expect(modelPrice(model, 'cache', 1)).toBeCloseTo(1_125_000)
  expect(modelPrice({ ...model, mode: 'per_call', per_call_price_micro: 40_000 }, 'call', 1)).toBe(40_000)
})

test('基准价以美元编辑、精确提交 micro，发布前显示影响确认', async ({ page }) => {
  let draft = 2_000_000, published = 2_000_000, publishes = 0
  const key = 'pricing_base_per_1m_micro'
  await page.addInitScript(() => {
    localStorage.setItem('okapi.key', 'fixture-base-key')
    localStorage.setItem('okapi.lang', 'zh-CN')
  })
  await page.route('**/*', async (route) => {
    const request = route.request(), path = new URL(request.url()).pathname
    if (request.isNavigationRequest()) return route.fulfill({ path: fileURLToPath(new URL('../dist/index.html', import.meta.url)), contentType: 'text/html' })
    if (!/^\/(api|admin|auth)\//.test(path)) return route.continue()
    let json: unknown = { data: [] }
    if (path === '/api/me') json = { user_id: 1, key_id: 1, role: 100, permissions: ['*'], group: 'default', balance_micro: 0 }
    if (path === '/admin/settings') {
      if (request.method() === 'POST') {
        expect(request.postDataJSON().key).toBe(key)
        draft = request.postDataJSON().value
        json = { ok: true, requires_publish: true }
      } else json = { data: [{ key, value: draft, published_value: published, is_secret: false, configured: true, updated_at: null }] }
    }
    if (path === '/admin/models') json = { data: [], total: 0, unpriced: 0, base_price_per_1m_micro: draft, published_base_price_per_1m_micro: published }
    if (path === '/admin/pricing/publish') { publishes++; published = draft; json = { epoch: publishes } }
    return route.fulfill({ json })
  })
  await page.goto('/admin/settings')
  await page.getByRole('tab', { name: '高级设置' }).click()
  const card = page.getByRole('article', { name: '倍率基准价', exact: true })
  await expect(card).toContainText('$2 / 1M tokens')
  await card.getByRole('button', { name: '配置' }).click()
  const dialog = page.getByRole('dialog', { name: '倍率基准价', exact: true })
  const input = dialog.getByLabel('倍率基准价', { exact: true })
  await expect(input).toHaveValue('2')
  for (const invalid of ['', '0', '-1', 'NaN', '2.1234567', '1000001']) {
    await input.fill(invalid)
    await expect(dialog.getByRole('button', { name: '保存', exact: true })).toBeDisabled()
  }
  await input.fill('3.123456')
  await page.screenshot({ path: 'test-results/pricing-base-setting.png', animations: 'disabled' })
  await dialog.getByRole('button', { name: '保存', exact: true }).click()
  await expect(dialog).toHaveCount(0)
  expect(draft).toBe(3_123_456)
  expect(published).toBe(2_000_000)
  await expect(card).toContainText('已发布：$2')
  await card.getByRole('link', { name: '发布定价' }).click()
  await expect(page.getByText('倍率基准价：$3.123456 / 1M tokens', { exact: true })).toBeVisible()
  await page.getByRole('button', { name: '发布定价' }).click()
  const confirm = page.getByRole('alertdialog', { name: '确认发布全站基准价变更' })
  await expect(confirm).toContainText('从 $2 调整为 $3.123456')
  expect(publishes).toBe(0)
  await page.screenshot({ path: 'test-results/pricing-base-confirm.png', animations: 'disabled' })
  await confirm.getByRole('button', { name: '发布定价', exact: true }).click()
  await expect.poll(() => publishes).toBe(1)
  await expect(page.getByText('已发布：$2 / 1M tokens', { exact: false })).toHaveCount(0)
})
