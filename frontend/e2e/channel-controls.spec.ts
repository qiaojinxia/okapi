import { expect, test } from '@playwright/test'
import type { Page } from '@playwright/test'
import { fileURLToPath } from 'node:url'

type Json = Record<string, unknown>
const usage = { timezone: 'America/Los_Angeles', usage: {
  window_start: '2026-10-01T07:00:00Z', window_end: '2026-10-02T07:00:00Z',
  requests: 12, tokens: 456, cost_micro: 1_234_567, unknown_cost_requests: 0,
}, token_usage: { tokens: 456, window_start: null, window_end: null }, quotas: [] }
const channel = { id: 42, name: 'controlled-channel', provider: 'openai', api_base: 'https://api.openai.com/v1',
  status: 1, priority: 0, models: ['mock-model'], keys: [], pools: ['default'], pool_members: [],
  cost_milli: 1000, data_retention: null, last_test: null, last_balance: null,
  settings: { extensions: {}, proxy_url: 'http://proxy.example:3128', account_control: {
    usage: { period: 'day', requests: 20, tokens: 10000, cost_micro: 1_234_567 },
  } },
}
const providerMetadata = { data: [
  { id: 'openai', account: { quota: false, refresh: false, subscription: null } },
  { id: 'openai_compat', account: { quota: false, refresh: false, subscription: null } },
  { id: 'anthropic', account: { quota: false, refresh: false, subscription: null } },
  { id: 'azure', account: { quota: false, refresh: false, subscription: null } },
  { id: 'gemini', account: { quota: false, refresh: false, subscription: null } },
  { id: 'bedrock', account: { quota: false, refresh: false, subscription: null } },
  { id: 'vertex', account: { quota: false, refresh: false, subscription: null } },
  { id: 'custom_pass', account: { quota: false, refresh: false, subscription: null } },
  { id: 'anthropic_max', account: { authorization: { code_format: 'code_state', access_token_prefix: 'sk-ant-oat', account_id_required: false, import_profile: { name: 'claude-code', mode: 'mimic', revision: '2.1.290' } }, quota: true, refresh: true, subscription: { quota_scope: 'session', window_secs: 18000, quota_windows: [18000, 604800] } } },
  { id: 'codex', account: { authorization: { code_format: 'callback_url', access_token_prefix: null, account_id_required: true }, quota: true, refresh: true, subscription: { quota_scope: 'total', window_secs: null, quota_windows: [18000, 604800] } } },
] }
async function prepare(page: Page, rows: Json[] = []) {
  await page.addInitScript(() => {
    localStorage.setItem('okapi.key', 'controls-ui-fixture')
    localStorage.setItem('okapi.lang', 'zh-CN')
  })
  await page.route('**/*', async (route) => {
    const request = route.request(), path = new URL(request.url()).pathname
    if (request.isNavigationRequest()) return route.fulfill({
      path: fileURLToPath(new URL('../dist/index.html', import.meta.url)), contentType: 'text/html',
    })
    if (/^\/(api|admin|auth)\//.test(path)) {
      expect(request.method(), `unmocked write: ${path}`).toBe('GET')
      return route.fulfill({ json: path === '/api/me' ? {
        user_id: 1, key_id: 1, group: 'default', balance_micro: 10_000_000, role: 100, permissions: ['*'],
      } : path === '/api/notice' ? { notice: null }
        : path === '/admin/channels/providers' ? providerMetadata
        : path === '/admin/channels' ? { data: rows, total: rows.length, enabled: rows.length }
        : path === '/admin/pools' ? { data: [poolRow('default')], total: 1 }
        : path === '/admin/channels/42/usage' ? usage
        : path.startsWith('/admin/settings/') ? { value: null } : { data: [] } })
    }
    return route.continue()
  })
  await page.goto('/admin/channels')
}
async function newChannel(page: Page, provider = 'openai', expandLimits = true) {
  await prepare(page)
  await page.getByRole('button', { name: '新建渠道', exact: true }).first().click()
  const drawer = page.getByRole('dialog')
  await expect(drawer.locator(':focus')).toHaveCount(1)
  await drawer.locator('#d-name').fill('new-controlled-channel')
  await drawer.locator('#d-provider').selectOption(provider)
  await drawer.locator('#d-models').fill('mock-model')
  await drawer.locator('#d-models').press('Enter')
  if (expandLimits) await drawer.locator('#channel-limits > summary').click()
  return drawer
}

test('API channels have only common controls and submit validated concurrency and pause settings', async ({ page }) => {
  const drawer = await newChannel(page)
  let saved: Json | undefined
  await page.route('**/admin/channels', async (route) => {
    if (route.request().method() === 'GET') return route.fallback()
    saved = route.request().postDataJSON()
    return route.fulfill({ json: { channel_id: 42, channel_key_id: 7 } })
  })
  await drawer.locator('#d-cred').fill('mock-api-key')
  await expect(drawer.locator('#d-provider option[value="anthropic_max"]')).toHaveText('Claude Code（订阅）')
  await expect(drawer.locator('#d-provider option[value="codex"]')).toHaveText('Codex（订阅）')
  for (const id of ['channel-subscription-controls', 'channel-quota-threshold', 'channel-limit-tokens', 'channel-limit-cost', 'channel-limit-requests', 'channel-usage-period']) {
    await expect(drawer.locator(`#${id}`)).toHaveCount(0)
  }
  const submit = drawer.getByRole('button', { name: '新建', exact: true })
  await drawer.locator('#d-concurrency').fill('2147483648')
  await expect(submit).toBeDisabled()
  await drawer.locator('#d-concurrency').fill('2')
  for (const bad of ['0', '1.5', '21', '']) {
    await drawer.locator('#channel-failure_threshold').fill(bad)
    await expect(submit).toBeDisabled()
    await expect(drawer.locator('#channel-failure_threshold')).toHaveAttribute('aria-invalid', 'true')
  }
  await drawer.locator('#channel-failure_threshold').fill('4')
  await drawer.locator('#channel-rate_limit_cooldown_secs').fill('300')
  await drawer.locator('#channel-failure_cooldown_secs').fill('90')
  await submit.click()
  await expect.poll(() => saved).toMatchObject({ max_concurrency: 2, settings: { account_control: {
    failure_threshold: 4, rate_limit_cooldown_secs: 300, failure_cooldown_secs: 90,
  } } })
  expect((saved!.settings as Json).account_control).not.toHaveProperty('usage')
})

function poolRow(code: string) {
  return { pool_code: code, description: null, routing_strategy: 'priority_weighted', fallback_pool_code: null,
    builtin: code === 'default', channel_count: 0, group_count: 0, key_count: 0, fallback_ref_count: 0 }
}
const keyRow = (id: number) => ({ id, status: 1, weight: 1, max_concurrency: null,
  cooldown_until: null, failed_count: 0, credential_kind: 0 })

async function openScheduling(page: Page) {
  await page.getByRole('row').filter({ hasText: channel.name }).getByRole('button', { name: '编辑', exact: true }).click()
  const drawer = page.getByRole('dialog')
  await expect(drawer.locator(':focus')).toHaveCount(1)
  await drawer.getByRole('tab', { name: '调度', exact: true }).click()
  return drawer
}

// 渠道池决定谁能调用这些模型，与模型同在一个页签
async function openPools(page: Page) {
  await page.getByRole('row').filter({ hasText: channel.name }).getByRole('button', { name: '编辑', exact: true }).click()
  const drawer = page.getByRole('dialog')
  await expect(drawer.locator(':focus')).toHaveCount(1)
  await showPools(drawer)
  return drawer
}

async function showPools(drawer: ReturnType<Page['getByRole']>) {
  await drawer.getByRole('tab', { name: '模型与渠道池', exact: true }).click()
  await drawer.locator('#channel-pools > summary').click()
}

test('a single credential hides its internal id and creates a selected pool without losing channel edits', async ({ page }) => {
  await prepare(page, [{ ...channel, settings: {}, keys: [keyRow(2)],
    pool_members: [{ pool_code: 'default', priority_override: null, weight_override: null }] }])
  const pools = [poolRow('default')]
  const calls: { path: string; body: Json }[] = []
  await page.route('**/admin/pools', async (route) => {
    if (route.request().method() === 'GET') return route.fulfill({ json: { data: pools, total: pools.length } })
    const body = route.request().postDataJSON()
    calls.push({ path: '/admin/pools', body })
    pools.push(poolRow(body.pool_code))
    return route.fulfill({ json: { ok: true } })
  })
  const drawer = await openScheduling(page)
  await expect(drawer.getByText('#2', { exact: true })).toHaveCount(0)
  await expect(drawer.getByRole('heading', { name: /渠道 key 参数/i })).toHaveCount(0)
  await drawer.locator('#channel-schedule-options > summary').click()
  await drawer.locator('#d-priority').fill('9')
  await drawer.locator('#channel-key-options-2 > summary').click()
  await drawer.locator('#kc-2').fill('3')
  await page.route('**/admin/channels/42/keys/2', async (route) => {
    calls.push({ path: '/admin/channels/42/keys/2', body: route.request().postDataJSON() })
    return route.fulfill({ json: { ok: true } })
  })
  await drawer.locator('#channel-key-options-2').getByRole('button', { name: '保存', exact: true }).click()
  await expect.poll(() => calls[0]).toEqual({ path: '/admin/channels/42/keys/2', body: { weight: 1, max_concurrency: 3 } })
  await showPools(drawer)
  await drawer.getByRole('button', { name: '新建池', exact: true }).click()
  await drawer.getByRole('textbox', { name: '池代码', exact: true }).fill(' fast ')
  await drawer.getByRole('button', { name: '创建并选中', exact: true }).click()
  await expect(drawer.getByRole('checkbox', { name: 'fast', exact: true })).toBeChecked()
  await expect(page.getByRole('dialog')).toHaveCount(1)
  await expect.poll(() => calls[1]).toEqual({ path: '/admin/pools', body: {
    pool_code: 'fast', description: '', routing_strategy: 'priority_weighted', fallback_pool_code: null,
  } })
  await page.route('**/admin/channels/42/pools', async (route) => {
    calls.push({ path: '/admin/channels/42/pools', body: route.request().postDataJSON() })
    return route.fulfill({ json: { ok: true, orphan: false } })
  })
  await drawer.getByRole('button', { name: '保存池成员关系', exact: true }).click()
  await expect.poll(() => calls[2]).toEqual({ path: '/admin/channels/42/pools', body: { pools: [
    { pool_code: 'default', priority_override: null, weight_override: null },
    { pool_code: 'fast', priority_override: null, weight_override: null },
  ] } })
  // 切页签与建池都不丢渠道表单里尚未保存的修改
  await drawer.getByRole('tab', { name: '调度', exact: true }).click()
  await drawer.locator('#channel-schedule-options > summary').click()
  await expect(drawer.locator('#d-priority')).toHaveValue('9')
})

test('inline pool creation rejects existing codes, preserves failed drafts and joins a new channel', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 })
  const drawer = await newChannel(page, 'openai', false)
  const pools = [poolRow('default')]
  let fail = true, failCatalog = false, posts = 0, saved: Json | undefined
  await page.route('**/admin/pools', async (route) => {
    if (route.request().method() === 'GET') return route.fulfill(failCatalog
      ? { status: 500, json: { error: { code: 'internal_error' } } }
      : { json: { data: pools, total: pools.length } })
    posts++
    if (fail) return route.fulfill({ status: 500, json: { error: { code: 'internal_error' } } })
    pools.push(poolRow(route.request().postDataJSON().pool_code))
    return route.fulfill({ json: { ok: true } })
  })
  await drawer.locator('#channel-pools > summary').click()
  await drawer.getByRole('button', { name: '新建池', exact: true }).click()
  const input = drawer.getByRole('textbox', { name: '池代码', exact: true })
  await input.fill('default')
  await expect(drawer.getByRole('alert').filter({ hasText: '此池已存在' })).toBeVisible()
  await expect(drawer.getByRole('button', { name: '创建并选中', exact: true })).toBeDisabled()
  expect(posts).toBe(0)
  await input.fill('stable')
  failCatalog = true
  await drawer.getByRole('button', { name: '创建并选中', exact: true }).click()
  await expect(drawer.getByRole('form', { name: '新建池' }).getByRole('alert')).toContainText('服务内部错误')
  await expect(input).toHaveValue('stable')
  expect(posts).toBe(0)
  failCatalog = false
  await drawer.getByRole('button', { name: '创建并选中', exact: true }).click()
  await expect.poll(() => posts).toBe(1)
  await expect(drawer.getByRole('form', { name: '新建池' }).getByRole('alert')).toContainText('服务内部错误')
  await expect(input).toHaveValue('stable')
  await expect(drawer.locator('#d-name')).toHaveValue('new-controlled-channel')
  expect(await drawer.evaluate((element) => element.scrollWidth <= element.clientWidth)).toBe(true)
  await page.screenshot({ path: '/tmp/okapi-channel-pool-inline-mobile.png' })
  fail = false
  await drawer.getByRole('button', { name: '创建并选中', exact: true }).click()
  await expect(drawer.getByRole('checkbox', { name: 'stable', exact: true })).toBeChecked()
  await drawer.locator('#d-cred').fill('mock-api-key')
  await page.route('**/admin/channels', async (route) => {
    if (route.request().method() === 'GET') return route.fallback()
    saved = route.request().postDataJSON()
    return route.fulfill({ json: { channel_id: 42, channel_key_id: 2 } })
  })
  await drawer.getByRole('button', { name: '新建', exact: true }).click()
  await expect.poll(() => saved).toMatchObject({ name: 'new-controlled-channel', pools: [
    { pool_code: 'default', priority_override: null, weight_override: null },
    { pool_code: 'stable', priority_override: null, weight_override: null },
  ] })
})

test('pool selectors load all pages and retained multi-credential channels keep distinguishable ids', async ({ page }) => {
  await prepare(page, [{ ...channel, settings: {}, keys: [keyRow(2), keyRow(3)] }])
  const paths: string[] = []
  const all = [poolRow('default'), ...Array.from({ length: 20 }, (_, i) => poolRow(`pool-${i + 1}`))]
  await page.route('**/admin/pools**', (route) => {
    expect(route.request().method()).toBe('GET')
    paths.push(new URL(route.request().url()).search)
    const offset = Number(new URL(route.request().url()).searchParams.get('offset') ?? 0)
    return route.fulfill({ json: { data: all.slice(offset, offset === 0 ? 20 : all.length), total: all.length } })
  })
  const drawer = await openPools(page)
  await expect(drawer.getByRole('checkbox', { name: 'pool-20', exact: true })).toBeVisible()
  expect(paths).toEqual(['', '?limit=200&offset=20'])
  await drawer.getByRole('tab', { name: '调度', exact: true }).click()
  await expect(drawer.getByText('#2', { exact: true })).toBeVisible()
  await expect(drawer.getByText('#3', { exact: true })).toBeVisible()
  await showPools(drawer)
  await drawer.getByRole('button', { name: '新建池', exact: true }).click()
  await drawer.getByRole('textbox', { name: '池代码', exact: true }).fill('pool-20')
  await expect(drawer.getByRole('button', { name: '创建并选中', exact: true })).toBeDisabled()
  await drawer.locator('form').getByRole('button', { name: '取消', exact: true }).click()
  await expect(drawer.getByRole('checkbox', { name: 'pool-20', exact: true })).not.toBeChecked()
})

test('pool loading errors offer retry', async ({ page }) => {
  await prepare(page, [{ ...channel, keys: [keyRow(2)], settings: {} }])
  let failed = true
  await page.route('**/admin/pools', (route) => route.fulfill(failed
    ? { status: 500, json: { error: { code: 'internal_error' } } }
    : { json: { data: [poolRow('default')], total: 1 } }))
  await page.reload()
  const drawer = await openPools(page)
  await expect(drawer.getByRole('alert').filter({ hasText: '服务内部错误' })).toBeVisible()
  failed = false
  await drawer.getByRole('button', { name: '重试', exact: true }).click()
  await expect(drawer.getByRole('checkbox', { name: 'default（内置默认池）' })).toBeVisible()
})

// 入池只有全站 channel.write 能做（后端 pools_require_all_scope）：own 范围看不到池编辑器、新建时也不发 pools
test('users without global channel write cannot change pool membership', async ({ page }) => {
  await prepare(page, [{ ...channel, keys: [keyRow(2)], settings: {} }])
  await page.route('**/api/me', (route) => route.fulfill({ json: {
    user_id: 1, key_id: 1, group: 'default', balance_micro: 10000000, role: 10,
    permissions: ['channel.read', 'channel.write.own'],
  } }))
  await page.reload()
  await page.getByRole('row').filter({ hasText: channel.name }).getByRole('button', { name: '编辑', exact: true }).click()
  const drawer = page.getByRole('dialog')
  await drawer.getByRole('tab', { name: '模型与渠道池', exact: true }).click()
  await expect(drawer.getByRole('note').filter({ hasText: '联系管理员审核' })).toBeVisible()
  await expect(drawer.locator('#channel-pools')).toHaveCount(0)
  await expect(drawer.getByRole('checkbox', { name: 'default（内置默认池）' })).toHaveCount(0)
})

test('editing removes retired local budgets and the retired proxy_url, preserving unrelated extension settings', async ({ page }) => {
  await prepare(page, [channel])
  let saved: Json | undefined
  await page.route('**/admin/channels/42', async (route) => {
    saved = route.request().postDataJSON()
    return route.fulfill({ json: { ok: true } })
  })
  await page.getByRole('row').filter({ hasText: channel.name }).getByRole('button', { name: '编辑', exact: true }).click()
  const drawer = page.getByRole('dialog')
  await drawer.getByRole('tab', { name: '调度', exact: true }).click()
  await expect(drawer.getByRole('note')).toContainText('原有本地累计额度限制已停用')
  await expect(drawer.locator('#channel-subscription-controls')).toHaveCount(0)
  await drawer.getByRole('button', { name: '保存', exact: true }).click()
  await expect.poll(() => saved).toMatchObject({ settings: { extensions: {} } })
  expect((saved!.settings as Json).account_control).not.toHaveProperty('usage')
  // settings.proxy_url 已退役（出口绑定取代），残留值不能被整体回写——后端会以 400 拒绝
  expect(saved!.settings as Json).not.toHaveProperty('proxy_url')
})

test('OAuth creation separates upstream percentage and authorization from common controls', async ({ page }) => {
  const drawer = await newChannel(page, 'anthropic_max')
  let exchanged: Json | undefined
  await page.route('**/admin/channels/oauth/start', (route) => route.fulfill({ json: {
    state: 'mock-state', authorize_url: 'about:blank', redirect_uri: 'http://localhost/callback',
  } }))
  await page.route('**/admin/channels/oauth/exchange', async (route) => {
    exchanged = route.request().postDataJSON()
    return route.fulfill({ json: { channel_id: 42, channel_key_id: 7, expires_at: 2000000000 } })
  })
  await drawer.locator('#channel-schedule-options > summary').click()
  await drawer.locator('#d-priority').fill('17')
  await drawer.locator('#d-cost').fill('0.25')
  await drawer.locator('#channel-subscription-controls > summary').click()
  await expect(drawer.getByLabel('5小时额度使用上限（%）', { exact: true })).toBeVisible()
  await drawer.locator('#channel-quota-18000').fill('85')
  await drawer.locator('#channel-quota-604800').fill('70')
  await drawer.locator('#channel-authorization-options > summary').click()
  await drawer.getByRole('switch', { name: '自动续期', exact: true }).uncheck()
  await expect(drawer.locator('#channel-refresh-margin')).toHaveCount(0)
  await drawer.locator('#channel-failure_threshold').fill('4')
  await drawer.locator('#d-concurrency').fill('3')
  await drawer.getByRole('button', { name: '打开登录页', exact: true }).click()
  await drawer.getByLabel('把浏览器里的 code 贴回来').fill('mock-code')
  await drawer.getByRole('button', { name: '换取凭证并创建渠道', exact: true }).click()
  await expect.poll(() => exchanged).toMatchObject({ name: 'new-controlled-channel', max_concurrency: 3,
    priority: 17, cost_milli: 250, api_base: '', pools: [{ pool_code: 'default', priority_override: null, weight_override: null }],
    settings: { account_control: { quota_aware: false, quota_limits: { '18000': 85, '604800': 70 }, refresh_mode: 'external', failure_threshold: 4 } },
  })
  expect((exchanged!.settings as Json).account_control).not.toHaveProperty('usage')
})

test('editing a subscription reauthorizes its original credential', async ({ page }) => {
  await prepare(page, [{ ...channel, provider: 'codex', settings: {}, keys: [{ id: 7, status: 1, weight: 10,
    max_concurrency: 2, cooldown_until: null, failed_count: 0, credential_kind: 1, oauth_refreshable: true }] }])
  const calls: unknown[] = []
  await page.route('**/admin/channels/oauth/start', async (route) => {
    calls.push(route.request().postDataJSON())
    return route.fulfill({ json: { state: 'bound-state', authorize_url: 'about:blank', redirect_uri: 'http://localhost/callback' } })
  })
  await page.route('**/admin/channels/oauth/exchange', async (route) => {
    calls.push(route.request().postDataJSON())
    return route.fulfill({ json: { channel_id: 42, channel_key_id: 7, expires_at: 2000000000 } })
  })
  await page.getByRole('row').filter({ hasText: channel.name }).getByRole('button', { name: '编辑', exact: true }).click()
  const drawer = page.getByRole('dialog')
  await drawer.getByRole('button', { name: '打开登录页', exact: true }).click()
  await drawer.getByLabel('把浏览器里的 code 贴回来').fill('replacement-code')
  await drawer.getByRole('button', { name: '换取凭证并更新此 key', exact: true }).click()
  await expect.poll(() => calls).toEqual([
    { provider: 'codex', channel_id: 42, channel_key_id: 7 },
    { state: 'bound-state', code: 'replacement-code', channel_id: 42, channel_key_id: 7 },
  ])
})

test('provider switching hides subscription controls and preserves common settings', async ({ page }) => {
  const drawer = await newChannel(page)
  await drawer.locator('#d-concurrency').fill('4')
  await drawer.locator('#channel-failure_threshold').fill('5')
  await drawer.locator('#d-provider').selectOption('codex')
  await drawer.locator('#channel-subscription-controls > summary').click()
  await expect(drawer.getByLabel('周额度使用上限（%）', { exact: true })).toBeVisible()
  await drawer.locator('#channel-quota-18000').fill('85')
  await drawer.locator('#d-provider').selectOption('openai')
  await expect(drawer.locator('#channel-subscription-controls')).toHaveCount(0)
  await expect(drawer.locator('#channel-failure_threshold')).toHaveValue('5')
  await expect(drawer.locator('#d-concurrency')).toHaveValue('4')
  await drawer.locator('#d-provider').selectOption('anthropic_max')
  await expect(drawer.getByLabel('5小时额度使用上限（%）', { exact: true })).toHaveValue('85')
})

test('subscription percentage and refresh numbers validate before authorization even when folded', async ({ page }) => {
  const drawer = await newChannel(page, 'codex')
  await drawer.locator('#channel-subscription-controls > summary').click()
  const authorize = drawer.getByRole('button', { name: '打开登录页', exact: true })
  for (const bad of ['0', '101', '1.5']) {
    await drawer.locator('#channel-quota-18000').fill(bad)
    await expect(authorize).toBeDisabled()
  }
  await drawer.locator('#channel-subscription-controls > summary').click()
  await expect(drawer.getByRole('alert')).toContainText('用量百分比请输入')
  await drawer.locator('#channel-subscription-controls > summary').click()
  await drawer.locator('#channel-quota-18000').fill('90')
  await drawer.locator('#channel-authorization-options > summary').click()
  await drawer.locator('#channel-renewal-advanced > summary').click()
  await drawer.locator('#channel-refresh-margin').fill('119')
  await expect(authorize).toBeDisabled()
  await drawer.getByRole('switch', { name: '自动续期', exact: true }).uncheck()
  await expect(authorize).toBeEnabled()
  await expect(drawer.locator('#channel-refresh-margin')).toHaveCount(0)
  await drawer.getByRole('switch', { name: '自动续期', exact: true }).check()
  await drawer.locator('#channel-renewal-advanced > summary').click()
  await expect(authorize).toBeDisabled()
  await drawer.locator('#channel-refresh-margin').fill('120')
  await drawer.locator('#channel-quota-18000').fill('')
  await expect(authorize).toBeEnabled()
  await expect(drawer.locator('#channel-usage-period')).toHaveCount(0)
})

test('an access-token import queries quota without offering automatic renewal', async ({ page }) => {
  const drawer = await newChannel(page, 'anthropic_max')
  let saved: Json | undefined
  await page.route('**/admin/channels', async (route) => {
    if (route.request().method() === 'GET') return route.fallback()
    saved = route.request().postDataJSON()
    return route.fulfill({ json: { channel_id: 42, channel_key_id: 7 } })
  })
  await drawer.getByRole('tab', { name: '直接导入 Token', exact: true }).click()
  await drawer.locator('#d-cred').fill('sk-ant-oat01-fixture')
  await drawer.locator('#channel-subscription-controls > summary').click()
  await drawer.locator('#channel-quota-18000').fill('80')
  await drawer.locator('#channel-local-token-controls > summary').click()
  await expect(drawer.locator('#channel-token-period')).toHaveValue('total')
  const submit = drawer.getByRole('button', { name: '新建', exact: true })
  for (const bad of ['0', '-1', '1.5', '9007199254740992']) {
    await drawer.locator('#channel-limit-tokens').fill(bad)
    await expect(submit).toBeDisabled()
  }
  await drawer.locator('#channel-limit-tokens').fill('1000000')
  await drawer.locator('#channel-token-period').selectOption('week')
  await expect(drawer.locator('#channel-limit-cost')).toHaveCount(0)
  await drawer.locator('#channel-authorization-options > summary').click()
  await expect(drawer.getByRole('switch', { name: '自动续期', exact: true })).toHaveCount(0)
  await expect(drawer.locator('#channel-refresh-margin')).toHaveCount(0)
  await expect(drawer.getByText(/仅导入访问 Token，无法自动续期/)).toBeVisible()
  await drawer.getByRole('button', { name: '新建', exact: true }).click()
  await expect.poll(() => saved).toMatchObject({ settings: { account_control: { quota_aware: false, quota_limits: { '18000': 80 }, local_tokens: { cap: 1000000, period: 'week' } } } })
})

test('subscription controls use plugin metadata rather than provider name', async ({ page }) => {
  await prepare(page)
  await page.route('**/admin/channels/providers', (route) => route.fulfill({ json: { data: [
    { id: 'openai', account: { quota: true, refresh: false, subscription: { quota_scope: 'session', window_secs: 7200, quota_windows: [7200] } } },
  ] } }))
  await page.getByRole('button', { name: '新建渠道', exact: true }).first().click()
  const drawer = page.getByRole('dialog')
  await drawer.locator('#channel-subscription-controls > summary').click()
  await expect(drawer.getByLabel('2小时额度使用上限（%）', { exact: true })).toBeVisible()
  await expect(drawer.getByRole('switch', { name: '自动续期', exact: true })).toHaveCount(0)
})

test('a newly registered account plugin drives channel options, authorization and token import', async ({ page }) => {
  await prepare(page)
  await page.route('**/admin/channels/providers', (route) => route.fulfill({ json: { data: [
    providerMetadata.data[0],
    { id: 'third_subscription', default_base: 'https://third.example/v1', account: {
      quota: true, refresh: true, subscription: { quota_scope: 'session', window_secs: 7200, quota_windows: [7200] },
      authorization: { code_format: 'callback_url', access_token_prefix: 'third_access_', account_id_required: false,
        import_profile: { name: 'native' } },
    } },
  ] } }))
  await page.getByRole('button', { name: '新建渠道', exact: true }).first().click()
  const drawer = page.getByRole('dialog')
  await drawer.locator('#d-provider').selectOption('third_subscription')
  await drawer.locator('#d-name').fill('third-account')
  await drawer.locator('#d-models').fill('third-model')
  await drawer.locator('#d-models').press('Enter')
  await expect(drawer.locator('#channel-connection-options')).not.toHaveAttribute('open', '')
  await page.route('**/admin/channels/oauth/start', (route) => route.fulfill({ json: {
    state: 'third-state', authorize_url: 'about:blank', redirect_uri: 'http://localhost:2345/callback',
  } }))
  await drawer.getByRole('button', { name: '打开登录页', exact: true }).click()
  await expect(drawer.locator('#oauth-code')).toHaveAttribute('placeholder', 'http://localhost:2345/callback?code=…')
  await expect(drawer.getByText(/授权后会跳转到 http:\/\/localhost:2345\/callback/)).toBeVisible()
  await drawer.getByRole('tab', { name: '直接导入 Token', exact: true }).click()
  await drawer.locator('#d-cred').fill('third_access_fixture')
  await drawer.locator('#channel-subscription-controls > summary').click()
  await drawer.locator('#channel-quota-7200').fill('80')
  await drawer.locator('#channel-authorization-options > summary').click()
  await expect(drawer.getByRole('switch', { name: '自动续期', exact: true })).toHaveCount(0)
  let saved: Json | undefined
  await page.route('**/admin/channels', (route) => {
    if (route.request().method() === 'GET') return route.fallback()
    saved = route.request().postDataJSON()
    return route.fulfill({ json: { channel_id: 43, channel_key_id: 8 } })
  })
  await drawer.getByRole('button', { name: '新建', exact: true }).click()
  await expect.poll(() => saved).toMatchObject({ provider: 'third_subscription', credential: 'third_access_fixture',
    settings: { extensions: { client_profile: { name: 'native' } }, account_control: { quota_limits: { '7200': 80 } } } })
})

test('folded subscription observations defer queries and recover errors without resetting drafts', async ({ page }) => {
  await page.clock.install()
  await prepare(page, [{ ...channel, provider: 'codex', settings: {}, keys: [{ ...keyRow(7), credential_kind: 1 }] }])
  let requests = 0, fail = true
  await page.route('**/admin/channels/42/usage*', (route) => {
    requests++
    return route.fulfill(fail ? { status: 500, json: { error: { code: 'internal_error' } } } : { json: usage })
  })
  // 列表行会查询一次额度来画用量条；抽屉里折叠的额度区块不再额外查询
  await expect.poll(() => requests).toBeGreaterThan(0)
  const listed = requests
  const drawer = await openScheduling(page)
  expect(requests).toBe(listed)
  await drawer.locator('#channel-subscription-controls > summary').click()
  await drawer.locator('#channel-quota-18000').fill('83')
  const section = drawer.locator('#channel-subscription-controls')
  // TanStack retries are finite; advance the fixture clock rather than waiting in real time.
  await page.clock.runFor(10_000)
  await expect(section.getByRole('alert')).toContainText('服务内部错误')
  fail = false
  await section.getByRole('button', { name: '重试', exact: true }).click()
  await expect(section.getByText('该周期已记录 456 Token', { exact: false })).toHaveCount(1)
  await expect(drawer.locator('#channel-quota-18000')).toHaveValue('83')
  await drawer.locator('#channel-subscription-controls > summary').click()
  await page.clock.runFor(50)
  const before = requests
  await page.clock.fastForward(45_000)
  // 列表行的额度单元格每 30s 自己刷新一次（与抽屉同一个查询键，fastForward 每个定时器最多触发一次）；
  // 折起的区块不能再多查。原先断言 0 次只是赶上请求晚于断言才过，时序一变就红
  expect(requests).toBeLessThanOrEqual(before + 1)
})

test('account catalog failure blocks creation and retry preserves the configuration draft', async ({ page }) => {
  await page.clock.install()
  await prepare(page)
  let fail = true
  await page.route('**/admin/channels/providers', (route) => route.fulfill(fail
    ? { status: 500, json: { error: { code: 'internal_error' } } }
    : { json: providerMetadata }))
  await page.getByRole('button', { name: '新建渠道', exact: true }).first().click()
  const drawer = page.getByRole('dialog')
  await drawer.locator('#d-name').fill('preserved-draft')
  await drawer.locator('#d-models').fill('mock-model')
  await drawer.locator('#d-models').press('Enter')
  await drawer.locator('#d-cred').fill('mock-api-key')
  await page.clock.runFor(10_000)
  await expect(drawer.getByRole('alert')).toContainText('服务内部错误')
  await expect(drawer.getByRole('button', { name: '新建', exact: true })).toBeDisabled()
  fail = false
  await drawer.getByRole('button', { name: '重试', exact: true }).click()
  await expect(drawer.getByRole('button', { name: '新建', exact: true })).toBeEnabled()
  await expect(drawer.locator('#d-name')).toHaveValue('preserved-draft')
  await expect(drawer.locator('#d-cred')).toHaveValue('mock-api-key')
})

test('a non-subscription quota hook does not expose subscription configuration', async ({ page }) => {
  await prepare(page)
  await page.route('**/admin/channels/providers', (route) => route.fulfill({ json: { data: [
    { id: 'openai', account: { quota: true, refresh: false, subscription: null } },
  ] } }))
  await page.getByRole('button', { name: '新建渠道', exact: true }).first().click()
  await expect(page.getByRole('dialog').locator('#channel-subscription-controls')).toHaveCount(0)
})

for (const provider of ['anthropic_max', 'codex']) {
  test(`${provider} automatic renewal switch saves both states and preserves independent usage controls`, async ({ page }) => {
    if (provider === 'anthropic_max') await page.setViewportSize({ width: 390, height: 844 })
    const account_control = { quota_aware: false, quota_threshold_pct: 90, quota_limits: { '18000': 85, '604800': 75 },
      local_tokens: { cap: 123456, period: 'day' }, refresh_mode: 'external', refresh_margin_secs: 600,
      failure_threshold: 4, rate_limit_cooldown_secs: 300 }
    const rows: Json[] = [{ ...channel, provider, settings: { account_control }, keys: [{ ...keyRow(7),
      credential_kind: 1, oauth_refreshable: true }] }]
    await prepare(page, rows)
    let saved: Json | undefined
    await page.route('**/admin/channels/42', async (route) => {
      saved = route.request().postDataJSON()
      rows[0] = { ...rows[0], settings: saved!.settings }
      return route.fulfill({ json: { ok: true } })
    })
    const openRenewal = async () => {
      const drawer = await openScheduling(page)
      await drawer.locator('#channel-subscription-controls > summary').click()
      await drawer.locator('#channel-authorization-options > summary').click()
      return drawer
    }
    let drawer = await openRenewal()
    let toggle = drawer.getByRole('switch', { name: '自动续期', exact: true })
    await expect(toggle).not.toBeChecked()
    await expect(drawer.locator('#channel-refresh-margin')).toHaveCount(0)
    await toggle.check()
    await expect(drawer.locator('#channel-renewal-advanced')).not.toHaveAttribute('open', '')
    await drawer.locator('#channel-renewal-advanced > summary').click()
    await expect(drawer.locator('#channel-refresh-margin')).toHaveValue('600')
    await drawer.locator('#channel-refresh-margin').fill('180')
    await drawer.locator('#channel-authorization-options > summary').click()
    await expect(drawer.locator('#channel-authorization-options > summary')).toContainText('自动续期已开启')
    await drawer.getByRole('button', { name: '保存', exact: true }).last().click()
    await expect.poll(() => saved).toMatchObject({ settings: { account_control: {
      ...account_control, refresh_mode: 'managed', refresh_margin_secs: 180,
    } } })
    await expect(drawer.getByRole('button', { name: '保存', exact: true }).last()).toBeEnabled()
    await page.reload()
    drawer = await openRenewal()
    toggle = drawer.getByRole('switch', { name: '自动续期', exact: true })
    await expect(toggle).toBeChecked()
    // 凭证状态与手动刷新属于接入信息
    await drawer.getByRole('tab', { name: '接入信息', exact: true }).click()
    await expect(drawer.getByRole('button', { name: '立即刷新凭证', exact: true })).toBeVisible()
    await drawer.getByRole('tab', { name: '调度', exact: true }).click()
    await drawer.locator('#channel-subscription-controls > summary').click()
    await drawer.locator('#channel-authorization-options > summary').click()
    toggle = drawer.getByRole('switch', { name: '自动续期', exact: true })
    await toggle.uncheck()
    await expect(drawer.locator('#channel-refresh-margin')).toHaveCount(0)
    await expect(drawer.getByText(/额度查询仍正常运行/)).toBeVisible()
    if (provider === 'anthropic_max') await page.screenshot({ path: '/tmp/okapi-auto-renewal-switch.png', animations: 'disabled' })
    await drawer.getByRole('button', { name: '保存', exact: true }).last().click()
    await expect.poll(() => saved).toMatchObject({ settings: { account_control: {
      ...account_control, refresh_mode: 'external', refresh_margin_secs: 180,
    } } })
    await expect(drawer.getByRole('button', { name: '保存', exact: true }).last()).toBeEnabled()
    await page.reload()
    drawer = await openRenewal()
    await expect(drawer.getByRole('switch', { name: '自动续期', exact: true })).not.toBeChecked()
    await drawer.getByRole('tab', { name: '接入信息', exact: true }).click()
    await expect(drawer.getByRole('button', { name: '立即刷新凭证', exact: true })).toHaveCount(0)
  })
}

test('external authorization management hides manual refresh and preserves subscription settings', async ({ page }) => {
  const account_control = { quota_aware: true, quota_threshold_pct: 75,
    refresh_mode: 'external', refresh_margin_secs: 600, failure_threshold: 5, rate_limit_cooldown_secs: 300 }
  await prepare(page, [{ ...channel, provider: 'codex', settings: { account_control }, keys: [{ id: 7, status: 1, weight: 10,
    max_concurrency: 2, cooldown_until: null, failed_count: 0, credential_kind: 1, oauth_refreshable: true }] }])
  let saved: Json | undefined
  await page.route('**/admin/channels/42', async (route) => {
    saved = route.request().postDataJSON()
    return route.fulfill({ json: { ok: true } })
  })
  await page.getByRole('row').filter({ hasText: channel.name }).getByRole('button', { name: '编辑', exact: true }).click()
  const drawer = page.getByRole('dialog')
  await expect(drawer.getByRole('button', { name: '立即刷新凭证', exact: true })).toHaveCount(0)
  await drawer.getByRole('tab', { name: '调度', exact: true }).click()
  await drawer.locator('#channel-subscription-controls > summary').click()
  await expect(drawer.locator('#channel-quota-604800')).toHaveValue('75')
  await drawer.locator('#channel-quota-604800').fill('80')
  await drawer.getByRole('button', { name: '保存', exact: true }).last().click()
  await expect.poll(() => saved).toMatchObject({ settings: { account_control: { ...account_control, quota_aware: false, quota_limits: { '604800': 80 } } } })
})

test('upstream quota shows its actual windows without any local period on mobile', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 })
  await prepare(page, [{ ...channel, provider: 'codex', settings: {} }])
  await page.route('**/admin/channels/42/usage*', (route) => route.fulfill({ json: { timezone: usage.timezone, quotas: [{ key_id: 7, quota: {
    observed_at: 2000000000, threshold_window: 'primary_window',
    windows: [
      { name: 'primary_window', used_percent: 45, window_secs: 30 * 86400, resets_at: 4070908800 },
      { name: 'expired_window', used_percent: 95, window_secs: 18000, resets_at: 1 },
    ],
  } }] } }))
  await page.getByRole('row').filter({ hasText: channel.name }).getByRole('button', { name: '编辑', exact: true }).click()
  const drawer = page.getByRole('dialog')
  await drawer.getByRole('tab', { name: '调度', exact: true }).click()
  await drawer.locator('#channel-subscription-controls > summary').click()
  const monthly = drawer.getByRole('progressbar', { name: '30天额度用量' })
  await expect(monthly).toHaveAttribute('aria-valuenow', '45')
  await expect(drawer.getByText('45%', { exact: true })).toBeVisible()
  await expect(drawer.getByText(/重置$/)).toBeVisible()
  await expect(drawer.getByText(/用于百分比上限/)).toHaveCount(0)
  // 已过重置时间的窗口不画旧百分比，提示等待上游刷新
  const session = drawer.getByRole('progressbar', { name: '5小时额度用量' })
  await expect(session).not.toHaveAttribute('aria-valuenow', /.*/)
  await expect(drawer.getByText('等待刷新', { exact: true })).toBeVisible()
  await expect(drawer.getByText('95%', { exact: true })).toHaveCount(0)
  await expect(drawer.locator('#channel-usage-period')).toHaveCount(0)
  await drawer.screenshot({ path: '/tmp/okapi-subscription-controls-mobile.png' })
  const dimensions = await drawer.evaluate((el) => ({ width: el.clientWidth, content: el.scrollWidth }))
  expect(dimensions.content).toBeLessThanOrEqual(dimensions.width)
})

test('default creation leaves all optional sections folded and submits unchanged defaults', async ({ page }) => {
  const drawer = await newChannel(page, 'anthropic', false)
  let saved: Json | undefined
  await page.route('**/admin/channels', async (route) => {
    if (route.request().method() === 'GET') return route.fallback()
    saved = route.request().postDataJSON()
    return route.fulfill({ json: { channel_id: 42, channel_key_id: 7 } })
  })
  for (const id of ['channel-connection-options', 'channel-client-options', 'channel-pools', 'channel-limits']) {
    await expect(drawer.locator(`#${id}`)).not.toHaveAttribute('open')
    await expect(drawer.locator(`#${id} > summary`)).toBeVisible()
  }
  await drawer.locator('#d-cred').fill('mock-api-key')
  await drawer.getByRole('button', { name: '新建', exact: true }).click()
  await expect.poll(() => saved).toEqual({
    name: 'new-controlled-channel', provider: 'anthropic', api_base: '', credential: 'mock-api-key',
    models: ['mock-model'], priority: 0, cost_milli: 1000, data_retention: '',
    settings: { thinking_to_content: false, bill_by_response_model: false, strip_request_fields: [] },
    pools: [{ pool_code: 'default', priority_override: null, weight_override: null }],
  })
})

test('folding common controls preserves drafts and keeps validation errors visible', async ({ page }) => {
  const drawer = await newChannel(page)
  await drawer.locator('#d-cred').fill('mock-api-key')
  await drawer.locator('#d-concurrency').fill('3')
  await drawer.locator('#channel-failure_threshold').fill('4')
  await drawer.locator('#channel-limits > summary').click()
  await expect(drawer.locator('#channel-limits > summary')).toContainText('并发上限 3')
  await expect(drawer.locator('#channel-limits > summary')).toContainText('连续失败 4 次')
  await drawer.locator('#channel-limits > summary').focus()
  await page.keyboard.press('Enter')
  await expect(drawer.locator('#channel-failure_threshold')).toHaveValue('4')
  await drawer.locator('#d-concurrency').fill('0')
  await drawer.locator('#channel-limits > summary').click()
  await expect(drawer.getByRole('alert')).toContainText('并发上限请输入')
  await expect(drawer.getByRole('button', { name: '新建', exact: true })).toBeDisabled()
  await drawer.locator('#channel-limits > summary').focus()
  await page.keyboard.press('Space')
  await drawer.locator('#d-concurrency').fill('3')
  await expect(drawer.getByRole('button', { name: '新建', exact: true })).toBeEnabled()
})

test('required cloud and pass-through addresses remain visible', async ({ page }) => {
  const drawer = await newChannel(page, 'openai', false)
  await expect(drawer.locator('#d-base')).not.toBeVisible()
  for (const provider of ['azure', 'bedrock', 'vertex', 'custom_pass']) {
    await drawer.locator('#d-provider').selectOption(provider)
    await drawer.locator('#d-cred').fill('mock-credential')
    await expect(drawer.locator('#d-base')).toBeVisible()
    await drawer.locator('#d-base').fill('')
    await expect(drawer.getByRole('button', { name: '新建', exact: true })).toBeDisabled()
    await drawer.locator('#d-base').fill('https://upstream.example.com')
    await expect(drawer.getByRole('button', { name: '新建', exact: true })).toBeEnabled()
  }
})
