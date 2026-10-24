import { expect, test } from '@playwright/test'
import type { Page, Route } from '@playwright/test'
import { fileURLToPath } from 'node:url'

type Json = Record<string, unknown>

const providerMetadata = { data: [
  { id: 'openai', account: { quota: false, refresh: false, subscription: null } },
  { id: 'anthropic', account: { quota: false, refresh: false, subscription: null } },
  { id: 'codex', account: { authorization: { code_format: 'callback_url', access_token_prefix: null, account_id_required: true }, quota: true, refresh: true, subscription: { quota_scope: 'total', window_secs: null, quota_windows: [18000, 604800] } } },
] }
// 价簿里只有 gpt-5 建了模型；其余发现到的名字都是「还没建」
const pricedModel = { model_name: 'gpt-5', display_name: null, vendor: 'OpenAI', pricing_mode: 'ratio' }

async function prepare(page: Page, options: { models?: 'ok' | 'denied' } = {}) {
  const calls: { path: string; method: string; body: Json | null }[] = []
  await page.addInitScript(() => {
    localStorage.setItem('okapi.key', 'fetch-models-fixture')
    localStorage.setItem('okapi.lang', 'zh-CN')
  })
  await page.route('**/*', async (route: Route) => {
    const request = route.request(), path = new URL(request.url()).pathname
    if (request.isNavigationRequest()) return route.fulfill({ path: fileURLToPath(new URL('../dist/index.html', import.meta.url)), contentType: 'text/html' })
    if (!/^\/(api|admin|auth)\//.test(path)) return route.continue()
    if (request.method() !== 'GET') {
      calls.push({ path, method: request.method(), body: (request.postDataJSON() ?? null) as Json | null })
      if (path === '/admin/channels/fetch-models') {
        const body = request.postDataJSON() as Json
        if (body.credential === 'sk-bad') return route.fulfill({ status: 502, json: { error: { code: 'upstream_error', param: 'status_401' } } })
        return route.fulfill({ json: { models: ['gpt-5', 'gpt-5-mini', 'o4'] } })
      }
      return route.fulfill({ json: { channel_id: 7, channel_key_id: 8 } })
    }
    if (path === '/admin/models' && options.models === 'denied') return route.fulfill({ status: 403, json: { error: { code: 'permission_denied', param: 'pricing.read' } } })
    return route.fulfill({ json: path === '/api/me' ? {
      user_id: 1, key_id: 1, group: 'default', balance_micro: 10_000_000, role: 100, permissions: ['*'],
    } : path === '/api/notice' ? { notice: null }
      : path === '/admin/channels/providers' ? providerMetadata
      : path === '/admin/channels' ? { data: [], total: 0, enabled: 0 }
      : path === '/admin/models' ? { data: [pricedModel], total: 1, unpriced: 0, base_price_per_1m_micro: 2_000_000 }
      : path === '/admin/pools' ? { data: [{ pool_code: 'default', description: null, routing_strategy: 'priority_weighted', fallback_pool_code: null, builtin: true, channel_count: 0, group_count: 0, key_count: 0, fallback_ref_count: 0 }], total: 1 }
      : path.startsWith('/admin/settings/') ? { value: null } : { data: [] } })
  })
  await page.goto('/admin/channels')
  return calls
}

async function openNew(page: Page) {
  await page.getByRole('button', { name: '新建渠道', exact: true }).first().click()
  const drawer = page.getByRole('dialog')
  await expect(drawer.locator(':focus')).toHaveCount(1)
  await drawer.locator('#d-name').fill('fresh-channel')
  return drawer
}

test('新建渠道还没保存时就能拉上游模型：凭证没填按钮不可用，填了用表单里的地址 / 凭证拉，并入清单且提示还没建价簿的名字', async ({ page }) => {
  const calls = await prepare(page)
  const drawer = await openNew(page)
  const fetchButton = drawer.getByRole('button', { name: '拉取上游模型' })
  await expect(fetchButton).toBeDisabled()
  await expect(drawer).toContainText('先填写凭证，再拉取上游模型')

  await drawer.locator('#d-models').fill('my-alias')
  await drawer.locator('#d-models').press('Enter')
  await drawer.locator('#d-cred').fill('sk-live')
  await expect(fetchButton).toBeEnabled()
  await fetchButton.click()

  await expect(page.getByRole('status').filter({ hasText: '发现 3 个模型，新增 3 个（已有的保留）' })).toBeVisible()
  expect(calls.filter((call) => call.path === '/admin/channels/fetch-models')).toEqual([
    { path: '/admin/channels/fetch-models', method: 'POST', body: expect.objectContaining({ provider: 'openai', credential: 'sk-live', api_base: '' }) },
  ])
  // gpt-5 在价簿里有模型；另外两个没有，提醒去补价（新开标签页，不丢这份草稿）
  const unpriced = drawer.getByTestId('discovered-unpriced')
  await expect(unpriced).toContainText('其中 2 个还没在价簿里建模型（gpt-5-mini, o4…）')
  await expect(unpriced.getByRole('link')).toHaveAttribute('href', '/admin/pricing')
  await expect(unpriced.getByRole('link')).toHaveAttribute('target', '_blank')

  // 手填的名字保留，发现的并在后面；再拉一次不会重复
  await fetchButton.click()
  await expect(page.getByRole('status').filter({ hasText: '发现 3 个模型，新增 0 个（已有的保留）' })).toBeVisible()
  await drawer.getByRole('button', { name: '新建', exact: true }).click()
  await expect.poll(() => calls.find((call) => call.path === '/admin/channels')?.body).toMatchObject({
    name: 'fresh-channel', models: ['my-alias', 'gpt-5', 'gpt-5-mini', 'o4'],
  })
})

test('拉取失败不动清单，错误提示出来；没有定价读取权限时照样能拉，只是不提示价簿缺口', async ({ page }) => {
  const calls = await prepare(page, { models: 'denied' })
  const drawer = await openNew(page)
  await drawer.locator('#d-cred').fill('sk-bad')
  await drawer.getByRole('button', { name: '拉取上游模型' }).click()
  await expect(page.getByRole('alert')).toBeVisible()
  await expect(drawer.getByTestId('discovered-unpriced')).toHaveCount(0)
  await expect(drawer.getByRole('button', { name: '新建', exact: true })).toBeDisabled()

  await drawer.locator('#d-cred').fill('sk-live')
  await drawer.getByRole('button', { name: '拉取上游模型' }).click()
  await expect(page.getByRole('status').filter({ hasText: '发现 3 个模型，新增 3 个' })).toBeVisible()
  await expect(drawer.getByTestId('discovered-unpriced')).toHaveCount(0)
  await expect(drawer.getByRole('button', { name: '新建', exact: true })).toBeEnabled()
  expect(calls.filter((call) => call.path === '/admin/channels/fetch-models')).toHaveLength(2)
})

test('订阅账号（登录换令牌）没有草稿凭证：不给按钮，说明先建渠道再拉', async ({ page }) => {
  const calls = await prepare(page)
  const drawer = await openNew(page)
  await drawer.locator('#d-provider').selectOption('codex')
  await expect(drawer.getByRole('button', { name: '拉取上游模型' })).toHaveCount(0)
  await expect(drawer).toContainText('订阅账号登录并创建渠道后，可在编辑页拉取上游模型')
  expect(calls.filter((call) => call.path === '/admin/channels/fetch-models')).toHaveLength(0)
})
