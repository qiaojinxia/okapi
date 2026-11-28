import { expect, test } from '@playwright/test'
import type { Page, Route } from '@playwright/test'
import { fileURLToPath } from 'node:url'
import { channelCreatePanel, nextStep, previousStep, toModelsStep } from './channel-steps'

type Json = Record<string, unknown>

const providerMetadata = { data: [
  { id: 'openai', default_base: 'https://api.openai.com/v1', account: { quota: false, refresh: false, subscription: null } },
  { id: 'anthropic', default_base: 'https://api.anthropic.com/v1', account: { quota: false, refresh: false, subscription: null } },
  { id: 'gemini', default_base: 'https://generativelanguage.googleapis.com/v1beta', account: { quota: false, refresh: false, subscription: null } },
  { id: 'codex', default_base: 'https://chatgpt.com/backend-api/codex', account: { authorization: { code_format: 'callback_url', access_token_prefix: null, account_id_required: true }, quota: true, refresh: true, subscription: { quota_scope: 'total', window_secs: null, quota_windows: [18000, 604800] } } },
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
        // 上游回了 200 + 网页（地址缺 /v1）：后端是 400 bad_request，param 区分
        if (body.credential === 'sk-page') return route.fulfill({ status: 400, json: { error: { code: 'bad_request', param: 'upstream_models_not_json' } } })
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
  const drawer = channelCreatePanel(page)
  await expect(drawer.locator(':focus')).toHaveCount(1)
  await drawer.locator('#d-name').fill('fresh-channel')
  return drawer
}

test('新建渠道还没保存时就能拉上游模型：凭证在第一步填，没填进不了模型这一步；填了用表单里的地址 / 凭证拉，并入清单且提示还没建价簿的名字', async ({ page }) => {
  const calls = await prepare(page)
  const drawer = await openNew(page)
  // 第一步没填凭证：「下一步」不可用，并说明还差什么
  const next = drawer.getByRole('button', { name: '下一步', exact: true })
  await expect(next).toBeDisabled()
  await expect(drawer.locator('[data-slot="step-hint"]')).toHaveText('请填写上游凭证。')
  await expect(drawer.getByRole('button', { name: '拉取上游模型' })).toBeHidden()
  await drawer.locator('#d-cred').fill('sk-live')
  await next.click()

  const fetchButton = drawer.getByRole('button', { name: '拉取上游模型' })
  await drawer.locator('#d-models').fill('my-alias')
  await drawer.locator('#d-models').press('Enter')
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

test('API 地址填的是 http:// 时提示明文传输（不分大小写、忽略首尾空白）；https 或留空不提示', async ({ page }) => {
  await prepare(page)
  const drawer = await openNew(page)
  await drawer.locator('#channel-connection-options > summary').click()
  const base = drawer.locator('#d-base')
  const warning = drawer.getByRole('note').filter({ hasText: 'HTTP 是明文传输' })
  await expect(warning).toHaveCount(0)
  await base.fill('http://38.92.24.38:8893/v1')
  await expect(warning).toBeVisible()
  await base.fill('https://relay.example.test/v1')
  await expect(warning).toHaveCount(0)
  await base.fill('  HTTP://relay.example.test:8893/v1 ')
  await expect(warning).toBeVisible()
  await base.fill('')
  await expect(warning).toHaveCount(0)
})

test('Anthropic / Gemini 的地址只写了主机名时提示要带版本段（/v1、/v1beta）；带了、留空、OpenAI 兼容、写不成地址都不提示', async ({ page }) => {
  await prepare(page)
  const drawer = await openNew(page)
  const base = drawer.locator('#d-base')
  const hint = drawer.getByRole('note').filter({ hasText: '这个协议的地址要带版本段' })
  // 「连接设置」折叠区：换协议会把它重新收起，每次要看地址框前先确保它是开的
  const openConnection = async () => {
    if (!(await base.isVisible())) await drawer.locator('#channel-connection-options > summary').click()
    await expect(base).toBeVisible()
  }
  await openConnection()

  // OpenAI 兼容：有的厂商本来就不带 /v1，不提示
  await base.fill('https://relay.example.test:8893')
  await expect(hint).toHaveCount(0)

  // 同一个只有主机名的地址，换成 Anthropic 协议就要提示
  await drawer.locator('#d-provider').selectOption('anthropic')
  await openConnection()
  await expect(hint).toBeVisible()
  await base.fill('http://38.92.24.38:8893')
  await expect(hint).toBeVisible()
  await expect(hint).toContainText('官方是 https://api.anthropic.com/v1')
  await base.fill('  https://relay.example.test:8893/  ')
  await expect(hint).toBeVisible()
  await base.fill('https://relay.example.test:8893/v1')
  await expect(hint).toHaveCount(0)
  await base.fill('https://relay.example.test/openai/anthropic')
  await expect(hint).toHaveCount(0)
  await base.fill('not a url')
  await expect(hint).toHaveCount(0)
  await base.fill('')
  await expect(hint).toHaveCount(0)

  await drawer.locator('#d-provider').selectOption('gemini')
  await openConnection()
  await base.fill('https://relay.example.test:8893')
  await expect(hint).toContainText('官方是 https://generativelanguage.googleapis.com/v1beta')
})

test('拉取上游回的是网页（地址缺 /v1）：提示原因，而不是「请求参数有误（upstream_models_not_json）」', async ({ page }) => {
  await prepare(page)
  const drawer = await openNew(page)
  await toModelsStep(drawer, { name: null, credential: 'sk-page' })
  await drawer.getByRole('button', { name: '拉取上游模型' }).click()
  const alert = page.getByRole('alert')
  await expect(alert).toContainText('上游返回的是网页而不是模型列表')
  await expect(alert).not.toContainText('upstream_models_not_json')
})

test('拉取失败不动清单，错误提示出来；没有定价读取权限时照样能拉，只是不提示价簿缺口', async ({ page }) => {
  const calls = await prepare(page, { models: 'denied' })
  const drawer = await openNew(page)
  await toModelsStep(drawer, { name: null, credential: 'sk-bad' })
  await drawer.getByRole('button', { name: '拉取上游模型' }).click()
  await expect(page.getByRole('alert')).toBeVisible()
  await expect(drawer.getByTestId('discovered-unpriced')).toHaveCount(0)
  await expect(drawer.getByRole('button', { name: '新建', exact: true })).toBeDisabled()

  // 回第一步换凭证再来：模型这一步已有的内容不丢
  await previousStep(drawer)
  await drawer.locator('#d-cred').fill('sk-live')
  await nextStep(drawer)
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
  // 订阅账号的第一步没有凭证可填；登录放在最后一步
  await expect(drawer.locator('#d-cred')).toHaveCount(0)
  await nextStep(drawer)
  await expect(drawer.getByRole('button', { name: '拉取上游模型' })).toHaveCount(0)
  await expect(drawer).toContainText('订阅账号登录并创建渠道后，可在编辑页拉取上游模型')
  expect(calls.filter((call) => call.path === '/admin/channels/fetch-models')).toHaveLength(0)
})

test('新建渠道以页内页签呈现：支持快捷协议卡片、切换列表页签保草稿，并可在引导模式与完整表单间无损切换', async ({ page }) => {
  await prepare(page)
  const createTab = page.getByRole('tab', { name: '新建渠道', exact: true })
  const listTab = page.getByRole('tab', { name: /渠道列表/ })
  await expect(listTab).toHaveAttribute('aria-selected', 'true')

  // 点击页级「新建渠道」页签：URL 同步 ?tab=create，非右侧 dialog 抽屉
  await createTab.click()
  await expect(page).toHaveURL(/tab=create/)
  await expect(page.getByRole('dialog')).toHaveCount(0)

  const panel = channelCreatePanel(page)
  await expect(panel).toBeVisible()
  // 点选快捷协议卡片，同步更新 #d-provider 与右侧摘要卡
  await panel.getByRole('group', { name: '常用协议快捷选择' }).getByRole('button', { name: 'anthropic', exact: true }).click()
  await expect(panel.locator('#d-provider')).toHaveValue('anthropic')
  await panel.locator('#d-name').fill('tab-draft-anthropic')
  await panel.locator('#d-cred').fill('sk-tab-draft')
  await expect(panel.locator('[data-slot="guided-summary"]')).toContainText('tab-draft-anthropic')

  // 切回列表页签再切回新建页签：草稿完整保留
  await listTab.click()
  await expect(page).not.toHaveURL(/tab=create/)
  await createTab.click()
  await expect(panel.locator('#d-name')).toHaveValue('tab-draft-anthropic')
  await expect(panel.locator('#d-cred')).toHaveValue('sk-tab-draft')

  // 切换到「完整表单」模式：步骤条收起，模型与调度区同屏可见且保留草稿；切回「引导模式」继续走步骤
  const modeToggle = panel.locator('[data-slot="channel-create-mode"]')
  await modeToggle.getByRole('button', { name: '完整表单', exact: true }).click()
  await expect(panel.getByRole('navigation', { name: '新建渠道的步骤' })).toHaveCount(0)
  await expect(panel.locator('#d-models')).toBeVisible()
  await panel.locator('#d-models').fill('claude-sonnet-4')
  await panel.locator('#d-models').press('Enter')
  await expect(panel.getByRole('button', { name: '新建', exact: true })).toBeEnabled()

  await modeToggle.getByRole('button', { name: '引导模式', exact: true }).click()
  await expect(panel.getByRole('navigation', { name: '新建渠道的步骤' })).toBeVisible()
  await expect(panel.locator('#d-name')).toHaveValue('tab-draft-anthropic')
})
