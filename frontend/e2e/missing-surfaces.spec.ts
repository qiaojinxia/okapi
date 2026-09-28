import { expect, test } from '@playwright/test'
import type { Download, Page } from '@playwright/test'
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

// 第 2.5 节此前没有写操作 e2e 的功能面：邀请返利页、令牌管理、导入抽屉
// （粘贴 JSON + 在线同步）、渠道抽屉的订阅 OAuth 登录卡；续补最近登录卡、
// 渠道健康时间线抽屉、审计页过滤进 URL、管理端日志过滤、路由诊断、注册入口、
// 门户日志、总览待办、消耗排行、日志行内退款、邮箱登录 TOTP、账户流水、
// 首启向导、服务质量卡、站点公告、API Key 登录 trim、门户总览查询、
// 站点规模条、用户用量抽屉、登出、OAuth 着陆兑 key、行内用量格子、
// 仅看未定价、总览 KPI / 实时条、语言与主题菜单、排行空态、退款幂等、
// 用量分析过滤条、运维退款未扣费、经营分组表、模型删除手输名称、
// 拆分聚焦、渠道 key 状态 / 近 24h、质量趋势、团队详情用量、
// 日志 CSV 六位 USD / 公式注入、测活徽章、用量 KPI 环比、
// 拆分毛利列、万元紧凑记法、用户列表搜索、渠道搜索、
// 兑换码停用整批、渠道池列表徽章、供应商控制台链接。
// 断言请求体 / 查询串形状。

type Json = Record<string, unknown>

async function prepare(page: Page, { permissions = ['*'], signedIn = true } = {}) {
  await page.addInitScript(
    ({ signedIn }) => {
      if (signedIn) localStorage.setItem('okapi.key', 'interaction-test-key')
      else localStorage.removeItem('okapi.key')
      localStorage.setItem('okapi.lang', 'zh-CN')
    },
    { signedIn },
  )
  await page.route('**/*', async (route) => {
    const request = route.request()
    const path = new URL(request.url()).pathname
    if (request.isNavigationRequest()) {
      await route.fulfill({
        path: fileURLToPath(new URL('../dist/index.html', import.meta.url)),
        contentType: 'text/html',
      })
    } else if (/^\/(api|admin|auth|pay)\//.test(path)) {
      expect(request.method(), `未被桩接住的写请求：${path}`).toBe('GET')
      const json: Json =
        path === '/api/me'
          ? {
              user_id: 1,
              key_id: 1,
              group: 'default',
              balance_micro: 10_000_000,
              balance_expires_at: null,
              role: permissions.length ? 100 : 1,
              permissions,
            }
          : path === '/api/notice'
            ? { notice: null }
            : path.startsWith('/admin/settings/')
              ? { value: null }
              : { data: [], next_before: null }
      await route.fulfill({ json })
    } else {
      await route.continue()
    }
  })
}

async function openedDialog(page: Page) {
  const dialog = page.getByRole('dialog')
  await expect(dialog.locator(':focus')).toHaveCount(1)
  return dialog
}

async function csvFrom(download: Download) {
  const path = await download.path()
  expect(path).toBeTruthy()
  const buf = readFileSync(path!)
  expect(buf.subarray(0, 3).equals(Buffer.from([0xef, 0xbb, 0xbf]))).toBe(true)
  return buf.toString('utf8').replace(/^\uFEFF/, '')
}

test('邀请返利页：链接带 aff 码、人数与累计返利按 micro 格式化；接口失败不伪装成零', async ({ page }) => {
  await prepare(page)
  await page.route('**/api/me/aff', (route) =>
    route.fulfill({
      json: { aff_code: 'ab12cd', invitees: 3, reward_sum_micro: 1_230_000 },
    }),
  )
  await page.goto('/portal/aff')
  await expect(page.getByRole('heading', { name: '邀请返利' })).toBeVisible()
  await expect(page.locator('code').filter({ hasText: '/?aff=ab12cd' })).toBeVisible()
  await expect(page.getByText('邀请码 ab12cd')).toBeVisible()
  await expect(page.getByText('已邀请')).toBeVisible()
  await expect(page.getByText('3', { exact: true }).first()).toBeVisible()
  await expect(page.getByText(/1\.23/)).toBeVisible()
  await expect(page.getByRole('button', { name: '复制邀请链接' }).first()).toBeVisible()
  await page.context().grantPermissions(['clipboard-read', 'clipboard-write'])
  await page.getByRole('button', { name: '复制邀请链接' }).first().click()
  await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toMatch(/\/\?aff=ab12cd$/)
  await expect(page.getByRole('status').filter({ hasText: '已复制' })).toBeVisible()

  await page.route('**/api/me/aff', (route) => route.fulfill(apiError(500, 'internal_error')))
  await page.reload()
  await expect(page.getByRole('alert')).toContainText('服务内部错误')
  await expect(page.getByRole('button', { name: '重试' })).toBeVisible()
})

function apiError(status: number, code: string, param?: string) {
  return { status, json: { error: { code, ...(param === undefined ? {} : { param }) } } }
}

test('令牌管理：检索条件进 URL；停用 / 启用 PATCH status；删除经确认框打 DELETE', async ({ page }) => {
  await prepare(page)
  const calls: { path: string; method: string; body: Json | null }[] = []
  const qs: URLSearchParams[] = []
  const row = {
    id: 9,
    user_id: 7,
    username: 'alice',
    team_id: null,
    name: 'ci-bot',
    key_prefix: 'sk-okapi-abcd',
    status: 1,
    quota_mode: 0,
    quota_micro: null,
    used_micro: 500_000,
    model_allowlist: ['gpt-5'],
    group_override: 'vip',
    ip_allowlist: ['10.0.0.1/32'],
    rpm_limit: 60,
    max_concurrency: null,
    expires_at: '2099-06-15T12:00:00Z',
    last_used_at: '2026-09-01T12:00:00Z',
    created_at: '2026-01-01T00:00:00Z',
  }
  await page.route('**/admin/keys?*', (route) => {
    if (route.request().method() !== 'GET') return route.fallback()
    const p = new URL(route.request().url()).searchParams
    qs.push(p)
    const q = p.get('q') ?? ''
    const data = q !== '' && !row.name.includes(q) && !row.key_prefix.includes(q) ? [] : [row]
    return route.fulfill({ json: { total: data.length, data } })
  })
  await page.route('**/admin/stats/entity-usage*', (route) =>
    route.fulfill({ json: { data: {} } }),
  )
  await page.route('**/admin/keys/9', async (route) => {
    const r = route.request()
    const body = (r.postDataJSON() ?? null) as Json | null
    calls.push({
      path: new URL(r.url()).pathname,
      method: r.method(),
      body,
    })
    if (r.method() === 'PATCH' && body && typeof body.status === 'number') row.status = body.status
    await route.fulfill({ json: { ok: true } })
  })

  await page.goto('/admin/keys')
  await expect(page.getByText('共 1 条')).toBeVisible()
  await expect(page.getByText('sk-okapi-abcd…')).toBeVisible()
  await expect(page.getByText('vip')).toBeVisible()
  await expect(page.getByText('限 1 个模型')).toBeVisible()
  await expect(page.getByText('限 1 个来源 IP')).toBeVisible()
  await expect(page.getByText(/\$0\.50/).first()).toBeVisible()
  await expect(page.getByText('2099-06-15')).toBeVisible()
  await expect(page.getByRole('link', { name: '查看这把令牌近 7 天的调用' })).toHaveAttribute('href', /\/admin\/logs\?.*api_key_id=9/)

  await page.getByLabel('搜索', { exact: true }).fill('ci-bot')
  await page.getByRole('combobox', { name: '所属用户', exact: true }).fill('7')
  await page.getByRole('button', { name: '搜索', exact: true }).click()
  await expect(page).toHaveURL(/q=ci-bot/)
  await expect(page).toHaveURL(/user_id=7/)
  await expect.poll(() => qs.some((p) => p.get('q') === 'ci-bot' && p.get('user_id') === '7')).toBe(true)

  await page.getByLabel('搜索', { exact: true }).fill('nope')
  await page.getByRole('button', { name: '搜索', exact: true }).click()
  await expect(page.getByText('没有匹配的结果', { exact: true })).toBeVisible()
  await page.getByLabel('搜索', { exact: true }).fill('ci-bot')
  await page.getByRole('button', { name: '搜索', exact: true }).click()
  await expect(page.getByText('sk-okapi-abcd…')).toBeVisible()

  await page.getByRole('button', { name: '停用', exact: true }).click()
  await expect.poll(() => calls).toEqual([
    { path: '/admin/keys/9', method: 'PATCH', body: { status: 2 } },
  ])
  await expect(page.getByRole('status').filter({ hasText: '操作成功' })).toBeVisible()
  await page.getByRole('status').getByRole('button', { name: '关闭', exact: true }).click()
  await page.getByRole('button', { name: '启用', exact: true }).click()
  await expect.poll(() => calls[1]).toEqual({
    path: '/admin/keys/9',
    method: 'PATCH',
    body: { status: 1 },
  })
  await page.getByRole('status').getByRole('button', { name: '关闭', exact: true }).click()
  await page.route(/\/admin\/keys\/\d+$/, (route) => {
    if (route.request().method() !== 'PATCH') return route.fallback()
    return route.fulfill(apiError(500, 'internal_error'))
  })
  await page.getByRole('button', { name: '停用', exact: true }).click()
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误' })).toBeVisible()
  await page.getByRole('alert').getByRole('button', { name: '关闭', exact: true }).click()

  await page.getByRole('button', { name: '删除', exact: true }).click()
  const confirm = page.getByRole('alertdialog')
  await expect(confirm).toContainText('sk-okapi-abcd')
  await page.route(/\/admin\/keys\/\d+$/, (route) => {
    if (route.request().method() !== 'DELETE') return route.fallback()
    return route.fulfill(apiError(500, 'internal_error'))
  })
  await confirm.getByRole('button', { name: '删除', exact: true }).click()
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误' })).toBeVisible()
  await page.getByRole('alert').getByRole('button', { name: '关闭', exact: true }).click()
  await page.route('**/admin/keys/9', async (route) => {
    const r = route.request()
    const body = (r.postDataJSON() ?? null) as Json | null
    calls.push({
      path: new URL(r.url()).pathname,
      method: r.method(),
      body,
    })
    if (r.method() === 'PATCH' && body && typeof body.status === 'number') row.status = body.status
    await route.fulfill({ json: { ok: true } })
  })
  await page.getByRole('button', { name: '删除', exact: true }).click()
  const retry = page.getByRole('alertdialog')
  await retry.getByRole('button', { name: '删除', exact: true }).click()
  await expect.poll(() => calls[2]).toEqual({
    path: '/admin/keys/9',
    method: 'DELETE',
    body: null,
  })

  await page.route('**/admin/keys?*', (route) => {
    if (route.request().isNavigationRequest() || route.request().method() !== 'GET') return route.fallback()
    return route.fulfill(apiError(500, 'internal_error'))
  })
  await page.getByLabel('搜索', { exact: true }).fill('boom')
  await page.getByRole('button', { name: '搜索', exact: true }).click()
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误，请稍后再试' })).toBeVisible()
  await expect(page.getByRole('button', { name: '重试' })).toBeVisible()
})

test('导入定价：粘贴 JSON 整段 POST；在线同步默认不选、点源值才发 apply，空源不放行拉取', async ({ page }) => {
  await prepare(page)
  const calls: { path: string; method: string; body: Json }[] = []
  await page.route('**/admin/models?*', (route) =>
    route.fulfill({ json: { data: [], total: 0, unpriced: 0 } }),
  )
  await page.route('**/admin/pricing/import-newapi', async (route) => {
    const r = route.request()
    calls.push({
      path: new URL(r.url()).pathname,
      method: r.method(),
      body: r.postDataJSON() as Json,
    })
    await route.fulfill({ json: { imported: 2, skipped: ['old'] } })
  })
  await page.route('**/admin/pricing/sync/fetch', async (route) => {
    const r = route.request()
    calls.push({
      path: new URL(r.url()).pathname,
      method: r.method(),
      body: r.postDataJSON() as Json,
    })
    await route.fulfill({
      json: {
        sources: [
          { name: 'up-a', status: 'ok', models: 2 },
          { name: 'up-b', status: 'error', error: 'timeout', models: 0 },
        ],
        differences: {
          'gpt-5': {
            model_ratio: { current: '1.250000', upstreams: { 'up-a': '1.500000', 'up-b': 'same' } },
            per_call_price: { current: null, upstreams: { 'up-a': '0.06' } },
          },
        },
      },
    })
  })
  await page.route('**/admin/pricing/sync/apply', async (route) => {
    const r = route.request()
    calls.push({
      path: new URL(r.url()).pathname,
      method: r.method(),
      body: r.postDataJSON() as Json,
    })
    await route.fulfill({ json: { applied: 1 } })
  })

  await page.goto('/admin/pricing')
  await page.getByRole('button', { name: '导入定价' }).click()
  const drawer = await openedDialog(page)

  // 粘贴页：空 JSON 不放行；非法 JSON 原地报错不发请求
  const run = drawer.getByRole('button', { name: '导入', exact: true })
  await expect(run).toBeDisabled()
  await drawer.locator('#import').fill('not-json')
  await run.click()
  await expect(page.getByRole('alert').filter({ hasText: 'JSON 格式错误' })).toBeVisible()
  expect(calls).toEqual([])
  await page.getByRole('alert').getByRole('button', { name: '关闭', exact: true }).click()
  await drawer.locator('#import').fill('{"models":[{"model_name":"gpt-5"}]}')
  const imported = page.waitForRequest((r) => r.url().includes('/import-newapi'))
  await run.click()
  await imported
  expect(calls[0]).toEqual({
    path: '/admin/pricing/import-newapi',
    method: 'POST',
    body: { models: [{ model_name: 'gpt-5' }] },
  })
  await expect(page.getByRole('status').filter({ hasText: '导入 2' })).toBeVisible()

  await drawer.getByRole('tab', { name: '在线同步' }).click()
  const fetchBtn = drawer.getByRole('button', { name: '拉取并对比' })
  await expect(fetchBtn).toBeDisabled()
  await drawer.locator('#sync-name-0').fill('up-a')
  await drawer.locator('#sync-url-0').fill('https://peer.example/api/pricing')
  await expect(fetchBtn).toBeEnabled()
  await drawer.getByRole('button', { name: '添加源' }).click()
  await drawer.locator('#sync-name-1').fill('up-b')
  await drawer.locator('#sync-url-1').fill('https://other.example/api/ratio_config')
  const fetched = page.waitForRequest((r) => r.url().includes('/sync/fetch'))
  await fetchBtn.click()
  await fetched
  expect(calls[1]).toEqual({
    path: '/admin/pricing/sync/fetch',
    method: 'POST',
    body: {
      sources: [
        { name: 'up-a', url: 'https://peer.example/api/pricing' },
        { name: 'up-b', url: 'https://other.example/api/ratio_config' },
      ],
    },
  })
  await expect(drawer.getByText('up-a · 2 个模型')).toBeVisible()
  await expect(drawer.getByText(/up-b · timeout/)).toBeVisible()
  await expect(drawer.getByText('1.25')).toBeVisible()
  await expect(drawer.getByText('未配置')).toBeVisible()

  const apply = drawer.getByRole('button', { name: /应用 0 项/ })
  await expect(apply).toBeDisabled()
  await drawer.getByRole('button', { name: '1.5', exact: true }).click()
  await expect(drawer.getByText('已选 1 项')).toBeVisible()
  await page.route('**/admin/pricing/sync/apply', (route) => route.fulfill(apiError(500, 'internal_error')))
  await drawer.getByRole('button', { name: '应用 1 项' }).click()
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误' })).toBeVisible()
  await page.getByRole('alert').getByRole('button', { name: '关闭', exact: true }).click()

  await page.route('**/admin/pricing/sync/apply', async (route) => {
    const r = route.request()
    calls.push({
      path: new URL(r.url()).pathname,
      method: r.method(),
      body: r.postDataJSON() as Json,
    })
    await route.fulfill({ json: { applied: 1 } })
  })
  const applied = page.waitForRequest((r) => r.url().includes('/sync/apply'))
  await drawer.getByRole('button', { name: '应用 1 项' }).click()
  await applied
  expect(calls[2]).toEqual({
    path: '/admin/pricing/sync/apply',
    method: 'POST',
    body: { changes: [{ model: 'gpt-5', axis: 'model_ratio', value: '1.500000' }] },
  })
  await expect(page.getByRole('status').filter({ hasText: '已应用 1 项' })).toBeVisible()

  await page.route('**/admin/pricing/sync/fetch', async (route) => {
    const r = route.request()
    calls.push({
      path: new URL(r.url()).pathname,
      method: r.method(),
      body: r.postDataJSON() as Json,
    })
    await route.fulfill({
      json: {
        sources: [{ name: 'up-a', status: 'ok', models: 2 }],
        differences: {},
      },
    })
  })
  await fetchBtn.click()
  await expect(drawer.getByText('拉到的值与本地定价全部一致，没有可应用的项。')).toBeVisible()

  await page.route('**/admin/pricing/sync/fetch', (route) => route.fulfill(apiError(500, 'internal_error')))
  await fetchBtn.click()
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误' })).toBeVisible()

  await drawer.getByRole('tab', { name: '粘贴 JSON' }).click()
  await page.route('**/admin/pricing/import-newapi', (route) => route.fulfill(apiError(500, 'internal_error')))
  await drawer.getByRole('button', { name: '导入', exact: true }).click()
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误' })).toBeVisible()
})

test('订阅 OAuth 登录卡：缺名/模型不换码；start 只带 provider；新建发 name+models，追加发 channel_id', async ({
  page,
}) => {
  await prepare(page)
  const calls: { path: string; method: string; body: Json }[] = []
  await page.route('**/admin/channels?*', (route) =>
    route.fulfill({
      json: {
        data: [
          {
            id: 42,
            name: 'max-home',
            provider: 'anthropic_max',
            api_base: 'https://api.anthropic.com',
            status: 1,
            priority: 0,
            models: ['claude-opus-4'],
            keys: [],
            settings: {},
            pools: ['default'],
            pool_members: [{ pool_code: 'default', priority_override: null, weight_override: null }],
            cost_milli: 1000,
            data_retention: null,
            last_test: null,
            last_balance: null,
          },
        ],
        total: 1,
        enabled: 1,
      },
    }),
  )
  await page.route('**/admin/pools', (route) => {
    if (route.request().isNavigationRequest()) return route.fallback()
    return route.fulfill({
      json: {
        data: [
          {
            pool_code: 'default',
            description: null,
            routing_strategy: 'priority_weighted',
            fallback_pool_code: null,
            builtin: true,
            channel_count: 1,
            group_count: 1,
            key_count: 0,
            fallback_ref_count: 0,
          },
        ],
      },
    })
  })
  await page.route('**/admin/channels/oauth/start', async (route) => {
    const r = route.request()
    calls.push({
      path: new URL(r.url()).pathname,
      method: r.method(),
      body: r.postDataJSON() as Json,
    })
    await route.fulfill({
      json: {
        authorize_url: 'https://claude.com/cai/oauth/authorize?state=st-1',
        state: 'st-1',
        redirect_uri: 'https://console.anthropic.com/oauth/code/callback',
      },
    })
  })
  await page.route('**/admin/channels/oauth/exchange', async (route) => {
    const r = route.request()
    calls.push({
      path: new URL(r.url()).pathname,
      method: r.method(),
      body: r.postDataJSON() as Json,
    })
    await route.fulfill({
      json: { channel_id: 42, channel_key_id: 8, expires_at: 1_800_000_000, account_id: null },
    })
  })

  await page.goto('/admin/channels')
  await page.getByRole('button', { name: '新建渠道' }).first().click()
  const create = await openedDialog(page)
  await create.locator('#d-provider').selectOption('anthropic_max')
  await expect(create.getByText('实验性')).toBeVisible()
  await create.getByRole('button', { name: '打开登录页' }).click()
  await expect.poll(() => calls[0]).toEqual({
    path: '/admin/channels/oauth/start',
    method: 'POST',
    body: { provider: 'anthropic_max' },
  })
  await expect(create.getByText('请先填渠道名和至少一个模型。')).toBeVisible()
  const submit = create.getByRole('button', { name: '换取凭证并创建渠道' })
  await expect(submit).toBeDisabled()
  await create.locator('#d-name').fill('max-new')
  const manual = create.locator('#d-models')
  await manual.fill('claude-opus-4')
  await manual.press('Enter')
  await create.locator('#oauth-code').fill(' code#st-1 ')
  const exchanged = page.waitForRequest((r) => r.url().includes('/oauth/exchange'))
  await submit.click()
  await exchanged
  expect(calls[1]).toEqual({
    path: '/admin/channels/oauth/exchange',
    method: 'POST',
    body: { state: 'st-1', code: 'code#st-1', name: 'max-new', models: ['claude-opus-4'] },
  })

  await page.keyboard.press('Escape')
  await page.getByRole('row').filter({ hasText: 'max-home' }).getByRole('button', { name: '编辑', exact: true }).click()
  const edit = await openedDialog(page)
  await expect(edit.locator('#d-provider')).toHaveValue('anthropic_max')
  await edit.getByRole('button', { name: '打开登录页' }).click()
  await expect.poll(() => calls[2]).toMatchObject({
    path: '/admin/channels/oauth/start',
    body: { provider: 'anthropic_max' },
  })
  await edit.locator('#oauth-code').fill('second-code')
  const attached = page.waitForRequest((r) => r.url().includes('/oauth/exchange'))
  await edit.getByRole('button', { name: '换取凭证并追加为一把 key' }).click()
  await attached
  expect(calls[3]).toEqual({
    path: '/admin/channels/oauth/exchange',
    method: 'POST',
    body: { state: 'st-1', code: 'second-code', channel_id: 42 },
  })

  await edit.getByRole('button', { name: '打开登录页' }).click()
  await expect.poll(() => calls[4]).toMatchObject({
    path: '/admin/channels/oauth/start',
    body: { provider: 'anthropic_max' },
  })
  await page.route('**/admin/channels/oauth/exchange', (route) => route.fulfill(apiError(500, 'internal_error')))
  await edit.locator('#oauth-code').fill('third-code')
  await edit.getByRole('button', { name: '换取凭证并追加为一把 key' }).click()
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误' })).toBeVisible()

  await page.route('**/admin/channels/oauth/start', (route) => route.fulfill(apiError(500, 'internal_error')))
  await edit.getByRole('button', { name: '重新打开登录页' }).click()
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误' })).toBeVisible()
})

test('最近登录：预览 8 行、失败原因、仅失败筛选、展开其余；500 走空态文案不伪装成零', async ({ page }) => {
  await prepare(page, { permissions: [] })
  const rows = Array.from({ length: 10 }, (_, i) => ({
    ok: i % 3 !== 0,
    at: `2026-09-08T0${i}:00:00Z`,
    ip: i % 3 === 0 ? '198.51.100.9' : '203.0.113.7',
    ua: 'Mozilla/5.0 Chrome/128 (Macintosh)',
    reason: i % 3 === 0 ? 'invalid_credentials' : null,
  }))
  await page.route('**/api/me/logins', (route) => route.fulfill({ json: { data: rows } }))

  await page.goto('/portal/security')
  const card = page.getByRole('heading', { name: '最近登录' }).locator('xpath=../..')
  await expect(card.getByRole('listitem')).toHaveCount(8)
  await expect(card.getByText('invalid_credentials').first()).toBeVisible()
  await expect(card.getByText('198.51.100.9').first()).toBeVisible()
  await expect(card.getByRole('button', { name: '展开其余 2 条' })).toBeVisible()

  await card.getByRole('button', { name: /仅看失败（4）/ }).click()
  await expect(card.getByRole('listitem')).toHaveCount(4)
  await expect(card.getByText('成功', { exact: true })).toHaveCount(0)
  await expect(card.getByText('203.0.113.7')).toHaveCount(0)
  await expect(card.getByRole('button', { name: /展开其余/ })).toHaveCount(0)

  await card.getByRole('button', { name: /仅看失败/ }).click()
  await card.getByRole('button', { name: '展开其余 2 条' }).click()
  await expect(card.getByRole('listitem')).toHaveCount(10)
  await card.getByRole('button', { name: '收起' }).click()
  await expect(card.getByRole('listitem')).toHaveCount(8)

  await page.route('**/api/me/logins', (route) => route.fulfill(apiError(500, 'internal_error')))
  await page.reload()
  const empty = page.getByRole('heading', { name: '最近登录' }).locator('xpath=../..')
  await expect(empty.getByText('还没有登录记录（API Key 登录不计入）。')).toBeVisible()
  await expect(empty.getByRole('listitem')).toHaveCount(0)
  await expect(empty.getByText('0', { exact: true })).toHaveCount(0)
})

test('渠道健康时间线：点近 24h 打开抽屉、hours 进查询、空态、深链带渠道与错误过滤', async ({ page }) => {
  await prepare(page)
  const timelineQs: string[] = []
  await page.route('**/admin/channels?*', (route) =>
    route.fulfill({
      json: {
        data: [
          {
            id: 42,
            name: 'openai-main',
            provider: 'openai',
            api_base: 'https://api.openai.com/v1',
            status: 1,
            priority: 0,
            models: ['gpt-5'],
            keys: [{ status: 1 }],
            settings: {},
            pools: ['default'],
            pool_members: [{ pool_code: 'default', priority_override: null, weight_override: null }],
            cost_milli: 1000,
            data_retention: null,
            last_test: null,
            last_balance: null,
          },
        ],
        total: 1,
        enabled: 1,
      },
    }),
  )
  await page.route('**/admin/stats/channels?*', (route) => {
    if (route.request().url().includes('/timeline')) return route.fallback()
    return route.fulfill({
      json: {
        data: [
          {
            channel_id: 42,
            name: 'openai-main',
            provider: 'openai',
            requests: 80,
            errors: 20,
            error_rate_bp: 2500,
            ttft_p50_ms: 120,
            ttft_p95_ms: 800,
            ttft_p99_ms: 900,
            failovers: 1,
            sticky_rate_bp: 0,
            tokens_per_1k_sec: 0,
            amount_micro: 0,
          },
        ],
      },
    })
  })
  let empty = true
  await page.route(/\/admin\/stats\/channels\/\d+\/timeline/, (route) => {
    const url = new URL(route.request().url())
    timelineQs.push(url.searchParams.get('hours') ?? '')
    if (empty) {
      return route.fulfill({
        json: { channel_id: 42, hours: Number(url.searchParams.get('hours') ?? 24), requests: 0, errors: 0, error_rate_bp: 0, data: [] },
      })
    }
    const now = new Date()
    const pad = (n: number) => String(n).padStart(2, '0')
    const bucket = `${now.getUTCFullYear()}-${pad(now.getUTCMonth() + 1)}-${pad(now.getUTCDate())} ${pad(now.getUTCHours())}:${pad(Math.floor(now.getUTCMinutes() / 5) * 5)}:00`
    return route.fulfill({
      json: {
        channel_id: 42,
        hours: Number(url.searchParams.get('hours') ?? 24),
        requests: 80,
        errors: 20,
        error_rate_bp: 2500,
        data: [
          {
            bucket,
            requests: 80,
            errors: 20,
            error_rate_bp: 2500,
            ttft_p50_ms: 120,
            ttft_p95_ms: 800,
            failovers: 1,
            tokens_per_1k_sec: 0,
          },
        ],
      },
    })
  })

  await page.goto('/admin/channels')
  await page.getByTitle(/健康时间线|Last-24h error rate/).click()
  const drawer = page.getByRole('dialog')
  await expect(drawer.getByRole('heading', { name: /openai-main/ })).toBeVisible()
  await expect(drawer.getByText('该时间段内这条渠道没有流量。')).toBeVisible()
  expect(timelineQs[0]).toBe('24')

  empty = false
  await drawer.getByRole('button', { name: '近 6 小时' }).click()
  await expect.poll(() => timelineQs.at(-1)).toBe('6')
  await expect(drawer.getByText('请求量（成功 / 失败）')).toBeVisible()
  await expect(drawer.getByText('首字时延', { exact: true })).toBeVisible()
  await expect(drawer.getByText(/25\.0/)).toBeVisible()
  const logs = drawer.getByRole('link', { name: '查看这段时间的日志' })
  await expect(logs).toHaveAttribute('href', /\/admin\/logs\?.*channel_id=42/)
  await expect(logs).toHaveAttribute('href', /hours=6/)
  await expect(logs).toHaveAttribute('href', /errors_only=true/)
  const analytics = drawer.getByRole('link', { name: '在用量分析中查看' })
  await expect(analytics).toHaveAttribute('href', /\/admin\/stats\?.*channel_id=42/)
  await expect(analytics).toHaveAttribute('href', /days=1/)

  await drawer.getByRole('button', { name: '近 7 天' }).click()
  await expect.poll(() => timelineQs.at(-1)).toBe('168')
  await expect(analytics).toHaveAttribute('href', /days=7/)

  await page.route(/\/admin\/stats\/channels\/\d+\/timeline/, (route) =>
    route.fulfill(apiError(500, 'internal_error')),
  )
  await drawer.getByRole('button', { name: '近 6 小时' }).click()
  await expect(drawer.getByRole('alert')).toContainText('服务内部错误')
})

for (const variant of [
  { width: 1440, theme: 'light', requests: 2, errors: 0 },
  { width: 1440, theme: 'dark', requests: 4, errors: 1 },
  { width: 390, theme: 'light', requests: 2, errors: 0 },
]) test(`渠道健康时间线可读性 ${variant.width} ${variant.theme}：稀疏请求柱不消失，汇总和堆叠数值准确`, async ({ page }) => {
  await prepare(page)
  await page.setViewportSize({ width: variant.width, height: 900 })
  await page.emulateMedia({ reducedMotion: 'reduce' })
  await page.addInitScript((theme) => localStorage.setItem('okapi.theme', theme), variant.theme)
  const now = Math.floor(Date.now() / 300000) * 300000
  const bucket = (minutes: number) => new Date(now - minutes * 60000).toISOString().slice(0, 19).replace('T', ' ')
  const health = { channel_id: 42, name: 'sparse-test', provider: 'openai', requests: variant.requests, errors: variant.errors,
    error_rate_bp: Math.round(variant.errors / variant.requests * 10000), ttft_p50_ms: 120, ttft_p95_ms: 800, ttft_p99_ms: 900,
    failovers: 0, sticky_rate_bp: 0, tokens_per_1k_sec: 0, amount_micro: 0 }
  await page.route('**/admin/channels?*', (route) => route.fulfill({ json: { data: [{
    id: 42, name: 'sparse-test', provider: 'openai', api_base: 'https://fixture.invalid/v1', status: 1, priority: 0,
    models: ['fixture-model'], keys: [{ status: 1 }], settings: {}, pools: ['default'],
    pool_members: [{ pool_code: 'default', priority_override: null, weight_override: null }],
    cost_milli: 1000, data_retention: null, last_test: null, last_balance: null,
  }], total: 1, enabled: 1 } }))
  await page.route('**/admin/stats/channels?*', (route) => route.fulfill({ json: { data: [health] } }))
  const windows: string[] = []
  await page.route(/\/admin\/stats\/channels\/42\/timeline/, (route) => {
    const hours = new URL(route.request().url()).searchParams.get('hours')!
    windows.push(hours)
    return route.fulfill({ json: { ...health, hours: Number(hours), data: [
      { ...health, bucket: bucket(95), requests: variant.requests - 1, errors: variant.errors },
      { ...health, bucket: bucket(15), requests: 1, errors: 0 },
    ] } })
  })
  await page.goto('/admin/channels')
  await page.getByTitle(/健康时间线|Last-24h error rate/).click()
  const drawer = page.getByRole('dialog')
  const section = drawer.getByRole('region', { name: '请求量（成功 / 失败）' })
  await expect(section.locator('dl > div').nth(0)).toHaveText(`总计${variant.requests}`)
  await expect(section.locator('dl > div').nth(1)).toHaveText(`成功${variant.requests - variant.errors}`)
  await expect(section.locator('dl > div').nth(2)).toHaveText(`失败${variant.errors}`)
  const bars = section.locator('.recharts-bar-rectangle path')
  await expect(bars).toHaveCount(variant.errors > 0 ? 3 : 2)
  for (const bar of await bars.all()) {
    const box = (await bar.boundingBox())!
    expect(box.width).toBeGreaterThan(variant.width > 500 ? 1.5 : 0.7)
    expect(box.height).toBeGreaterThan(20)
    await expect(bar).toHaveAttribute('stroke-width', '0.75')
  }
  expect(await drawer.evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true)
  if (variant.width > 500) {
    await bars.first().hover()
    await expect(section.locator('.recharts-tooltip-wrapper')).toContainText(`成功 : ${variant.requests - 1 - variant.errors}`)
    await expect(section.locator('.recharts-tooltip-wrapper')).toContainText(`失败 : ${variant.errors}`)
  }
  await drawer.getByRole('heading').hover()
  await page.screenshot({ path: `test-results/channel-timeline-${variant.width}-${variant.theme}.png`, animations: 'disabled' })
  // Switching windows keeps totals intact; the 7-day view still uses the existing hourly aggregation.
  for (const [name, hours] of [['近 6 小时', '6'], ['近 7 天', '168']]) {
    await drawer.getByRole('button', { name }).click()
    await expect.poll(() => windows.at(-1)).toBe(hours)
    await expect(section.locator('dl > div').nth(0)).toHaveText(`总计${variant.requests}`)
    await expect(bars).toHaveCount(variant.errors > 0 ? 3 : 2)
  }
})

test('审计页：空态、条件进 URL、行展开多出的 detail、加载更多带 before 游标', async ({ page }) => {
  await prepare(page)
  const queries: URL[] = []
  const row = (id: number, extra: Record<string, unknown> = {}) => ({
    id,
    actor: 'admin:1',
    actor_info: { kind: 'admin', id: 1, label: 'root' },
    action: 'channel.delete',
    target: '42',
    detail: { reason: 'dup', count: 3, extra: 'hidden-key', ...extra },
    ip: '203.0.113.7',
    created_at: '2026-09-08T12:00:00Z',
  })
  let page1 = { data: [row(20), row(19, { extra: 'second' })], has_more: true, next_before: 19 }
  await page.route('**/admin/audit/actions', (route) =>
    route.fulfill({ json: { data: ['channel.delete', 'user.login'] } }),
  )
  await page.route('**/admin/audit?*', (route) => {
    queries.push(new URL(route.request().url()))
    const before = new URL(route.request().url()).searchParams.get('before')
    if (before === '19') {
      return route.fulfill({ json: { data: [row(18)], has_more: false, next_before: null } })
    }
    return route.fulfill({ json: page1 })
  })

  await page.goto('/admin/audit')
  await expect(page.locator('#main-content').getByRole('heading', { name: '审计日志' })).toBeVisible()
  await expect(page.getByText('已加载 2 条')).toBeVisible()
  await expect(page.getByText('root').first()).toBeVisible()
  await expect(page.getByText('管理员', { exact: false }).first()).toBeVisible()
  await expect(page.getByText(/reason=dup/).first()).toBeVisible()
  await expect(page.getByText(/\+1/).first()).toBeVisible()
  await expect(page.getByText('hidden-key')).toHaveCount(0)

  await page.getByRole('row').filter({ hasText: 'channel.delete' }).first().click()
  await expect(page.getByText('hidden-key')).toBeVisible()

  await page.locator('#au-action').selectOption('user.')
  await page.locator('#au-target').fill(' root@ok.test ')
  await page.locator('#au-actor').fill('admin:1')
  await page.getByRole('button', { name: '搜索', exact: true }).click()
  await expect(page).toHaveURL(/action=user\./)
  await expect(page).toHaveURL(/target=root(%40|@)ok\.test/)
  await expect(page).toHaveURL(/actor=admin(:|%3A)1/)
  await expect.poll(() => {
    const last = queries.at(-1)
    return last && last.searchParams.get('action') === 'user.' && last.searchParams.get('target') === 'root@ok.test'
  }).toBeTruthy()

  await page.getByRole('button', { name: '近 24 小时' }).click()
  await expect(page).toHaveURL(/hours=24/)
  await expect.poll(() => queries.at(-1)?.searchParams.get('hours')).toBe('24')

  page1 = { data: [], has_more: false, next_before: null }
  await page.locator('#au-target').fill('nobody')
  await page.getByRole('button', { name: '搜索', exact: true }).click()
  await expect(page).toHaveURL(/target=nobody/)
  await expect(page.getByText('该条件下没有记录。审计只记写操作与登录，只读浏览不会出现在这里。')).toBeVisible()

  page1 = { data: [row(20), row(19, { extra: 'second' })], has_more: true, next_before: 19 }
  await page.goto('/admin/audit')
  await expect(page.getByText('已加载 2 条')).toBeVisible()
  await page.getByRole('button', { name: '加载更多' }).click()
  await expect.poll(() => queries.some((u) => u.searchParams.get('before') === '19')).toBe(true)
  await expect(page.getByText('已加载 3 条')).toBeVisible()

  await page.route('**/admin/audit?*', (route) => route.fulfill(apiError(500, 'internal_error')))
  await page.reload()
  await expect(page.getByRole('alert')).toContainText('服务内部错误')
  await expect(page.getByRole('button', { name: '重试' })).toBeVisible()
})

function adminLogRow() {
  return {
    ts: '2026-09-08 12:00:00',
    request_id: 'req-abc',
    upstream_request_id: 'up-1',
    log_type: 1,
    user_id: 7,
    username: 'alice',
    api_key_id: 9,
    group: 'vip',
    model: 'gpt-5',
    channel_id: 42,
    channel_name: 'openai-main',
    channel_key_id: 1,
    provider: 'openai',
    client_type: 'sdk',
    client_ip: '203.0.113.7',
    node: 'gw-1',
    usage: { prompt_tokens: 100, cached_tokens: 10, completion_tokens: 50, reasoning_tokens: 0 },
    amount_micro: 240,
    original_amount_micro: 300,
    discount_micro: 60,
    upstream_cost_micro: 100,
    latency_ms: 800,
    ttft_ms: 120,
    is_stream: true,
    retry_count: 0,
    failover_count: 0,
    sticky_layer: 0,
    upstream_status: 200,
    error_code: '',
    is_error: false,
    ratio_snapshot: '1',
  }
}

test('管理端日志：过滤进 URL 与查询串；7 天改 hours；展开 request_id；空表禁用导出', async ({ page }) => {
  await prepare(page)
  const queries: { path: string; qs: URLSearchParams }[] = []
  await page.route(/\/admin\/logs(\/stat)?\?/, (route) => {
    const url = new URL(route.request().url())
    queries.push({ path: url.pathname, qs: url.searchParams })
    if (url.pathname.endsWith('/stat')) {
      return route.fulfill({
        json: {
          requests: 12,
          errors: 1,
          error_rate_bp: 800,
          tokens: 1500,
          amount_micro: 240,
          discount_micro: 0,
          users: 2,
          cached_tokens: 10,
          cache_hit_bp: 1000,
          rpm: 3,
          tpm: 400,
          rate_source: 'clickhouse',
        },
      })
    }
    return route.fulfill({ json: { data: [{ ...adminLogRow(), error_code: '=1+1' }] } })
  })

  await page.goto('/admin/logs')
  await expect(page.locator('#main-content').getByRole('heading', { name: '日志' })).toBeVisible()
  await expect(page.getByText('alice')).toBeVisible()
  await expect(page.getByText('RPM', { exact: true })).toBeVisible()

  await page.locator('#lf-model').fill(' gpt-5 ')
  await page.getByText('更多筛选', { exact: true }).click()
  await page.locator('#lf-user_id').fill('7')
  await page.locator('#lf-channel_id').fill('42')
  await page.locator('#lf-request_id').fill(' req-abc ')
  await page.getByRole('switch', { name: '只看失败' }).click()
  await page.getByRole('button', { name: '搜索', exact: true }).click()
  await expect(page).toHaveURL(/model=gpt-5/)
  await expect(page).toHaveURL(/user_id=7/)
  await expect(page).toHaveURL(/channel_id=42/)
  await expect(page).toHaveURL(/request_id=req-abc/)
  await expect(page).toHaveURL(/errors_only=true/)
  await expect.poll(() =>
    queries.some(
      (q) =>
        q.path === '/admin/logs' &&
        q.qs.get('model') === 'gpt-5' &&
        q.qs.get('user_id') === '7' &&
        q.qs.get('channel_id') === '42' &&
        q.qs.get('request_id') === 'req-abc' &&
        q.qs.get('errors_only') === 'true',
    ),
  ).toBe(true)
  await expect.poll(() =>
    queries.some(
      (q) =>
        q.path === '/admin/logs/stat' &&
        q.qs.get('model') === 'gpt-5' &&
        q.qs.get('errors_only') === 'true',
    ),
  ).toBe(true)

  await page.getByRole('button', { name: '7 天', exact: true }).click()
  await expect(page).toHaveURL(/hours=168/)
  await expect.poll(() => queries.some((q) => q.qs.get('hours') === '168')).toBe(true)

  await page.getByRole('row').filter({ hasText: 'alice' }).first().click()
  await expect(page.getByText('req-abc')).toBeVisible()
  await expect(page.getByText('up-1')).toBeVisible()
  await page.context().grantPermissions(['clipboard-read', 'clipboard-write'])
  await page.getByRole('button', { name: '复制', exact: true }).first().click()
  await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toBe('req-abc')

  const [download] = await Promise.all([
    page.waitForEvent('download'),
    page.getByRole('button', { name: '导出 CSV' }).click(),
  ])
  expect(download.suggestedFilename()).toMatch(/^okapi-admin-logs-/)
  const csv = await csvFrom(download)
  expect(csv.split('\n')[0]).toBe(
    'time,status,error_code,user_id,username,api_key_id,group,model,channel_id,channel_name,provider,client_type,prompt_tokens,cached_tokens,completion_tokens,reasoning_tokens,amount_usd,original_usd,discount_usd,latency_ms,ttft_ms,stream,retry_count,failover_count,upstream_status,request_id,upstream_request_id,node,key_name,key_prefix',
  )
  expect(csv).toContain('ok,')
  expect(csv).toContain("'=1+1")
  expect(csv).toContain(',0.000240,0.000300,0.000060,')
  expect(csv).toContain(',req-abc,up-1,gw-1')

  await page.route(/\/admin\/logs(\/stat)?\?/, (route) => {
    const url = new URL(route.request().url())
    if (url.pathname.endsWith('/stat')) {
      return route.fulfill({
        json: {
          requests: 0,
          errors: 0,
          error_rate_bp: 0,
          tokens: 0,
          amount_micro: 0,
          discount_micro: 0,
          users: 0,
          cached_tokens: 0,
          cache_hit_bp: 0,
          rpm: 0,
          tpm: 0,
          rate_source: 'clickhouse',
        },
      })
    }
    return route.fulfill({ json: { data: [] } })
  })
  await page.getByRole('button', { name: '清空筛选' }).click()
  await page.getByRole('button', { name: '搜索', exact: true }).click()
  await expect(page.getByText('当前窗口与过滤条件下没有日志。放宽时间窗或清空过滤再试。')).toBeVisible()
  await expect(page.getByRole('button', { name: '导出 CSV' })).toBeDisabled()

  await page.route(/\/admin\/logs(\/stat)?\?/, (route) => {
    if (route.request().isNavigationRequest()) return route.fallback()
    const url = new URL(route.request().url())
    if (url.pathname.endsWith('/stat')) {
      return route.fulfill({
        json: {
          requests: 0, errors: 0, error_rate_bp: 0, tokens: 0, amount_micro: 0,
          discount_micro: 0, users: 0, cached_tokens: 0, cache_hit_bp: 0, rpm: 0, tpm: 0,
          rate_source: 'clickhouse',
        },
      })
    }
    return route.fulfill(apiError(500, 'internal_error'))
  })
  await page.locator('#lf-model').fill('gpt-4o')
  await page.getByRole('button', { name: '搜索', exact: true }).click()
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误，请稍后再试' })).toBeVisible()
  await expect(page.getByRole('button', { name: '重试' })).toBeVisible()
})

test('路由诊断：缺模型禁用；查询带 model/group/pool；结论、淘汰原因与降级池', async ({ page }) => {
  await prepare(page)
  const qs: string[] = []
  await page.route('**/admin/groups', (route) =>
    route.fulfill({ json: { data: [{ group_code: 'vip' }], total: 1 } }),
  )
  await page.route('**/admin/pools', (route) =>
    route.fulfill({ json: { data: [{ pool_code: 'default' }, { pool_code: 'spare' }], total: 2 } }),
  )
  await page.route('**/admin/diagnose/route?*', (route) => {
    const url = new URL(route.request().url())
    qs.push(url.search)
    return route.fulfill({
      json: {
        model: {
          requested: 'gpt-5',
          canonical: 'gpt-5',
          active: true,
          priced: true,
          via_alias: false,
          fallback_models: ['gpt-4o'],
        },
        scope: {
          group_code: 'vip',
          group_ratio: '0.8',
          pool_code: 'default',
          pool_source: 'group',
          pool_chain: ['default', 'spare'],
          routing_strategy: 'priority',
        },
        channels: [
          {
            channel_id: 42,
            name: 'openai-main',
            provider: 'openai',
            status: 2,
            priority: 0,
            pools: ['default'],
            via_fallback: true,
            excluded: 'channel_disabled',
            keys: [{ key_id: 1, status: 1, cooldown_until: null, weight: 1, ok: false, reason: 'key_cooling' }],
          },
        ],
        candidates: 0,
        verdict: 'no_available_channel',
        fallbacks: [{ model: 'gpt-4o', viable: true, candidates: 2, reason: null }],
      },
    })
  })
  await page.route('**/admin/channels?*', (route) =>
    route.fulfill({ json: { data: [], total: 0, enabled: 0 } }),
  )

  await page.goto('/admin/channels')
  await page.getByRole('button', { name: '路由诊断', exact: true }).click()
  const drawer = page.getByRole('dialog')
  await expect(drawer.getByRole('heading', { name: '路由诊断' })).toBeVisible()
  await expect(drawer.getByRole('button', { name: '诊断', exact: true })).toBeDisabled()

  await drawer.locator('#diag-model').fill(' gpt-5 ')
  await drawer.locator('#diag-group').selectOption('vip')
  await drawer.locator('#diag-pool').selectOption('spare')
  await drawer.getByRole('button', { name: '诊断', exact: true }).click()
  await expect.poll(() => qs.at(-1)).toContain('model=gpt-5')
  expect(qs.at(-1)).toContain('group=vip')
  expect(qs.at(-1)).toContain('pool=spare')
  expect(qs.at(-1)).toContain('ingress=chat_completions')
  await expect(drawer.getByText('零可用候选')).toBeVisible()
  await expect(drawer.getByText('0 个可用候选')).toBeVisible()
  await expect(drawer.getByText('渠道已停用')).toBeVisible()
  await expect(drawer.getByText('经降级池可见')).toBeVisible()
  await expect(drawer.getByText('冷却中')).toBeVisible()

  await page.route('**/admin/diagnose/route?*', (route) => route.fulfill(apiError(500, 'internal_error')))
  await drawer.getByRole('button', { name: '诊断', exact: true }).click()
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误' })).toBeVisible()
})

test('路由诊断按入口区分协议错误，Codex 配置显示限制', async ({ page }) => {
  await prepare(page)
  await page.route('**/admin/diagnose/route?*', (route) => {
    const responses = new URL(route.request().url()).searchParams.get('ingress') === 'responses'
    return route.fulfill({ json: {
      model: { requested: 'gpt-test', canonical: 'gpt-test', active: true, priced: true, via_alias: false, fallback_models: [] },
      scope: { group_code: null, pool_code: 'default', pool_chain: ['default'], routing_strategy: 'priority_weighted' },
      endpoint: responses ? '/v1/responses' : '/v1/chat/completions', available_endpoints: ['/v1/responses'],
      channels: [], candidates: responses ? 1 : 0, verdict: responses ? 'ok' : 'unsupported_endpoint', fallbacks: [],
    } })
  })
  await page.goto('/admin/channels')
  await page.getByRole('button', { name: '路由诊断', exact: true }).click()
  const drawer = page.getByRole('dialog')
  await drawer.locator('#diag-model').fill('gpt-test')
  await drawer.getByRole('button', { name: '诊断', exact: true }).click()
  await expect(drawer.getByRole('alert')).toContainText('/v1/responses')
  await drawer.getByLabel('实际调用接口').selectOption('responses')
  await expect(drawer.getByRole('alert')).toHaveCount(0)
  await drawer.getByRole('button', { name: '诊断', exact: true }).click()
  await expect(drawer.getByText('1 个可用候选')).toBeVisible()
  await page.keyboard.press('Escape')
  await page.getByRole('button', { name: '新建渠道' }).first().click()
  await drawer.locator('#d-provider').selectOption('codex')
  await expect(drawer.getByRole('note')).toContainText('/v1/responses')
})

test('注册：关闭不摆表单；邀请制必填 aff；URL aff 不手填；验证码带 lang；OAuth 只跳 /auth/oauth/{code}', async ({
  page,
}) => {
  await prepare(page, { signedIn: false })
  let policy: Json = { mode: 'closed', new_user_credit_micro: 0, invitee_credit_micro: 0, allowed_domains: [], email_verification: false }
  await page.route('**/api/registration', (route) => route.fulfill({ json: policy }))
  await page.route('**/auth/oauth-providers', (route) =>
    route.fulfill({ json: { providers: ['github', 'linuxdo'] } }),
  )

  await page.goto('/')
  await expect(page.getByRole('button', { name: '使用 github 登录' })).toBeVisible()
  await expect(page.getByRole('button', { name: '使用 linuxdo 登录' })).toBeVisible()
  const oauthNav = page.waitForRequest((r) => r.isNavigationRequest() && /\/auth\/oauth\/github$/.test(new URL(r.url()).pathname))
  await page.getByRole('button', { name: '使用 github 登录' }).click()
  const oauthReq = await oauthNav
  expect(new URL(oauthReq.url()).pathname).toBe('/auth/oauth/github')

  await page.goto('/')
  await page.getByRole('button', { name: '还没有账号？立即注册' }).click()
  await expect(page.getByText('本站暂不开放注册。如需账号请联系站长。')).toBeVisible()
  await expect(page.locator('#reg-email')).toHaveCount(0)

  policy = {
    mode: 'invite_only',
    new_user_credit_micro: 1_000_000,
    invitee_credit_micro: 500_000,
    allowed_domains: ['ok.test'],
    email_verification: true,
  }
  const posts: { path: string; body: Json }[] = []
  await page.route('**/auth/email-code', async (route) => {
    posts.push({ path: '/auth/email-code', body: route.request().postDataJSON() as Json })
    await route.fulfill({ json: { ok: true } })
  })
  await page.route('**/auth/register', async (route) => {
    posts.push({ path: '/auth/register', body: route.request().postDataJSON() as Json })
    await route.fulfill({ json: { ok: true } })
  })
  await page.route('**/auth/login', async (route) => {
    posts.push({ path: '/auth/login', body: route.request().postDataJSON() as Json })
    await route.fulfill({ json: { ok: true } })
  })
  await page.route('**/auth/keys', async (route) => {
    posts.push({ path: '/auth/keys', body: route.request().postDataJSON() as Json })
    await route.fulfill({ json: { api_key: 'sk-okapi-new', key_id: 2 } })
  })

  await page.goto('/')
  await page.getByRole('button', { name: '还没有账号？立即注册' }).click()
  await expect(page.getByText(/仅接受以下邮箱域名注册：ok\.test/)).toBeVisible()
  await expect(page.getByText(/注册即送/)).toContainText(/\$1\.00/)
  const submit = page.locator('form').getByRole('button', { name: '注册' })
  await expect(submit).toBeDisabled()
  await page.locator('#reg-email').fill('  new@ok.test  ')
  await page.getByRole('button', { name: '获取验证码' }).click()
  await expect.poll(() => posts.find((p) => p.path === '/auth/email-code')?.body).toEqual({
    email: 'new@ok.test',
    lang: 'zh-CN',
  })
  await page.locator('#reg-code').fill('123456')
  await page.locator('#reg-username').fill(' newbie ')
  await page.locator('#reg-password').fill('password1')
  await expect(submit).toBeDisabled()
  await page.locator('#reg-aff').fill('  ab12  ')
  await expect(page.getByText(/注册即送/)).toContainText(/\$1\.50/)
  await submit.click()
  await expect.poll(() => posts.some((p) => p.path === '/auth/register')).toBe(true)
  expect(posts.find((p) => p.path === '/auth/register')?.body).toEqual({
    email: 'new@ok.test',
    username: 'newbie',
    password: 'password1',
    aff_code: 'ab12',
    email_code: '123456',
  })
  await expect(page).toHaveURL(/\/portal/)

  await page.goto('/?aff=fromlink')
  await page.getByRole('button', { name: '还没有账号？立即注册' }).click()
  await expect(page.getByText('已应用邀请码 fromlink')).toBeVisible()
  await expect(page.locator('#reg-aff')).toHaveCount(0)
  await expect(page.getByText(/注册即送/)).toContainText(/\$1\.50/)
})

function portalLog(id: number, extra: Partial<{ model: string; status: number; error_code: string | null }> = {}) {
  return {
    id,
    request_id: `req-${id}`,
    model: extra.model ?? 'gpt-5',
    log_type: 2,
    status: extra.status ?? 20,
    api_key_id: 1,
    key_name: 'web',
    usage: { prompt_tokens: 100, cached_tokens: 20, completion_tokens: 40, reasoning_tokens: 8 },
    amount_micro: 240_000,
    original_amount_micro: 300_000,
    discount_micro: 60_000,
    pricing_snapshot: {
      mode: 'ratio',
      model_ratio: '1.25',
      completion_ratio: '8',
      cache_ratio: '0.1',
      group: 'vip',
      group_ratio: '0.8',
      user_multiplier: '1',
      rules: [{ code: 'night', kind: 'time', multiplier: '0.9' }],
    },
    error_code: extra.error_code ?? null,
    latency_ms: 800,
    ttft_ms: 120,
    is_stream: true,
    created_at: '2026-09-08T12:00:00Z',
  }
}

test('门户日志：范围/模型/失败进查询；展开账单快照；加载更多 before；空表禁导出', async ({ page }) => {
  await prepare(page)
  const qs: URLSearchParams[] = []
  let page1 = {
    scope: 'key',
    data: Array.from({ length: 50 }, (_, i) => portalLog(50 - i)),
    next_before: 1,
  }
  await page.route('**/api/me/logs?*', (route) => {
    const url = new URL(route.request().url())
    qs.push(url.searchParams)
    if (url.searchParams.get('before') === '1') {
      return route.fulfill({ json: { scope: 'user', data: [portalLog(1, { model: 'claude-4' })], next_before: null } })
    }
    return route.fulfill({ json: page1 })
  })

  await page.goto('/portal/logs')
  await expect(page.locator('#main-content').getByRole('heading', { name: '用量日志' })).toBeVisible()
  await expect(page.getByText('已加载 50 条')).toBeVisible()
  await expect.poll(() => qs[0]?.get('scope')).toBe('key')

  await page.getByRole('row').filter({ hasText: 'gpt-5' }).first().click()
  await expect(page.getByRole('dialog').getByText('优惠前金额')).toBeVisible()
  await expect(page.getByRole('dialog').getByText('实际消费')).toBeVisible()
  await expect(page.getByText(/night ×0\.9/)).toBeVisible()
  await expect(page.getByText('req-50')).toBeVisible()
  await page.context().grantPermissions(['clipboard-read', 'clipboard-write'])
  await page.getByRole('button', { name: '复制', exact: true }).click()
  await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toBe('req-50')
  await page.getByRole('dialog').getByRole('button', { name: '关闭', exact: true }).click()

  await page.getByRole('button', { name: '全账户' }).click()
  await expect.poll(() => qs.some((p) => p.get('scope') === 'user')).toBe(true)
  await page.getByPlaceholder('搜索模型、别名或厂商').fill(' gpt-5 ')
  await page.getByPlaceholder('搜索模型、别名或厂商').press('Enter')
  await page.getByRole('switch', { name: '只看失败' }).click()
  await expect.poll(() =>
    qs.some((p) => p.get('model') === 'gpt-5' && p.get('errors_only') === 'true' && p.get('scope') === 'user'),
  ).toBe(true)

  const [download] = await Promise.all([
    page.waitForEvent('download'),
    page.getByRole('button', { name: '导出已加载 CSV' }).click(),
  ])
  expect(download.suggestedFilename()).toMatch(/^okapi-usage-/)

  await page.getByRole('button', { name: '加载更多' }).click()
  await expect.poll(() => qs.some((p) => p.get('before') === '1')).toBe(true)
  await expect(page.getByText('已加载 51 条')).toBeVisible()

  page1 = { scope: 'key', data: [], next_before: null }
  await page.getByRole('button', { name: '本密钥' }).click()
  await expect(page.getByText('当前范围和时段内还没有调用。发起请求后，等待用量入库即可查看。')).toBeVisible()
  await expect(page.getByRole('button', { name: '导出已加载 CSV' })).toBeDisabled()

  await page.route('**/api/me/logs?*', (route) => {
    if (route.request().isNavigationRequest()) return route.fallback()
    return route.fulfill(apiError(500, 'internal_error'))
  })
  await page.getByRole('button', { name: '全账户' }).click()
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误，请稍后再试' })).toBeVisible()
  await expect(page.getByRole('button', { name: '重试' })).toBeVisible()
})

test('总览待办：健康芯片、全清文案、死信/未定价/空池/高错误率深链；切天数重拉渠道', async ({ page }) => {
  await prepare(page)
  const channelDays: string[] = []
  const diagnose = {
    postgres: false,
    redis: false,
    clickhouse: false,
    nats_connected: false,
    outbox_pending: 1000,
    dlq_depth: 3,
    cooling_keys: 2,
    pricebook_epoch: 4,
  }
  await page.route('**/admin/diagnose', (route) => route.fulfill({ json: diagnose }))
  await page.route('**/admin/stats/realtime*', (route) =>
    route.fulfill({
      json: {
        window_secs: 60,
        qps_milli: 0,
        requests: 0,
        errors: 0,
        error_rate_bp: 0,
        tokens: 0,
        amount_micro: 0,
        series: [],
      },
    }),
  )
  await page.route('**/admin/models', (route) =>
    route.fulfill({ json: { data: [{ model_name: 'gpt-5', pricing_mode: null }], total: 1, unpriced: 1 } }),
  )
  await page.route('**/admin/pools', (route) =>
    route.fulfill({ json: { data: [{ pool_code: 'empty-pool', channel_count: 0 }], total: 1 } }),
  )
  await page.route('**/admin/reconciliation', (route) =>
    route.fulfill({ json: { drift_count: 1, drifts: [{ user_id: 7, username: 'alice', events_sum_micro: 1, redis_effective_micro: 0, pg_snapshot_micro: 0 }] } }),
  )
  await page.route('**/admin/stats/channels?*', (route) => {
    channelDays.push(new URL(route.request().url()).searchParams.get('days') ?? '')
    return route.fulfill({
      json: { data: [{ channel_id: 42, name: 'openai-main', error_rate_bp: 2500 }] },
    })
  })
  await page.route('**/admin/stats/inventory', (route) =>
    route.fulfill({
      json: {
        users: { total: 1, active: 1, new_today: 0, new_7d: 0 },
        api_keys: { total: 1, active: 1, used_7d: 0 },
        channels: { total: 1, healthy: 1, no_key: 0, auto_disabled: 0, disabled: 0 },
        channel_keys: { active: 1, cooling: 0, rate_limited: 0, quota_exhausted: 0, banned: 0, invalid: 0 },
        models: { total: 1, priced: 0, served: 0 },
        groups: 1,
      },
    }),
  )

  await page.goto('/admin')
  const card = page.getByRole('region', { name: '优先处理', exact: true })
  const healthChips = page.getByRole('region', { name: '系统连接状态', exact: true })
  await expect(healthChips.getByText('PG', { exact: true })).toBeVisible()
  await expect(healthChips.getByText('Redis', { exact: true })).toBeVisible()
  await expect(healthChips.getByText('CH', { exact: true })).toBeVisible()
  await card.getByRole('button', { name: '全部 8 项', exact: true }).click()
  await expect(card.getByText(/PostgreSQL \/ Redis \/ ClickHouse 不可达/)).toBeVisible()
  await expect(card.getByText(/死信队列有 3 条/)).toBeVisible()
  await expect(card.getByText(/outbox 积压 1000 条/)).toBeVisible()
  await expect(card.getByText(/2 把渠道 key 处于冷却/)).toBeVisible()
  await expect(card.getByText(/1 个模型未配定价（如 gpt-5）/)).toBeVisible()
  await expect(card.getByText(/1 个渠道池是空的（如 empty-pool）/)).toBeVisible()
  await expect(card.getByText(/1 个渠道错误率超 5%（如 openai-main）/)).toBeVisible()
  await expect(card.getByText(/1 个用户三方对账存在漂移/)).toBeVisible()
  await expect(card.getByRole('link', { name: /未配定价/ })).toHaveAttribute('href', '/admin/pricing')
  await expect(card.getByRole('link', { name: /渠道池是空的/ })).toHaveAttribute('href', '/admin/pools')
  await expect(card.getByRole('link', { name: /死信队列/ })).toHaveAttribute('href', '/admin/ops')
  await expect(card.getByRole('link', { name: /错误率超 5%/ })).toHaveAttribute('href', '/admin/quality?days=7&tab=channels')
  expect(channelDays[0]).toBe('7')
  await page.getByRole('button', { name: '近 30 天' }).click()
  await expect.poll(() => channelDays.at(-1)).toBe('30')
  await expect(card.getByRole('link', { name: /错误率超 5%/ })).toHaveAttribute('href', '/admin/quality?days=30&tab=channels')

  diagnose.postgres = true
  diagnose.redis = true
  diagnose.nats_connected = true
  diagnose.clickhouse = null
  diagnose.dlq_depth = 0
  diagnose.cooling_keys = 0
  diagnose.outbox_pending = 0
  await page.route('**/admin/models', (route) => route.fulfill({ json: { data: [], total: 0, unpriced: 0 } }))
  await page.route('**/admin/pools', (route) => route.fulfill({ json: { data: [{ pool_code: 'default', channel_count: 2 }], total: 1 } }))
  await page.route('**/admin/reconciliation', (route) => route.fulfill({ json: { drift_count: 0, drifts: [] } }))
  await page.route('**/admin/stats/channels?*', (route) => route.fulfill({ json: { data: [] } }))
  await page.route('**/admin/stats/inventory', (route) => route.fulfill({ json: {
    users: { total: 1, active: 1, new_today: 0, new_7d: 0 },
    api_keys: { total: 1, active: 1, used_7d: 0 },
    channels: { total: 1, healthy: 1, no_key: 0, auto_disabled: 0, disabled: 0 },
    channel_keys: { active: 1, cooling: 0, rate_limited: 0, quota_exhausted: 0, banned: 0, invalid: 0 },
    models: { total: 1, priced: 1, served: 1 }, groups: 1,
  } }))
  await page.reload()
  await expect(page.getByText('没有待办，一切正常。')).toBeVisible()

  await page.route('**/admin/diagnose', (route) => {
    if (route.request().isNavigationRequest()) return route.fallback()
    return route.fulfill(apiError(500, 'internal_error'))
  })
  await page.reload()
  await expect(page.getByText('没有待办，一切正常。')).toHaveCount(0)
  await expect(page.getByText('部分检查未完成，暂时无法确认全部状态。')).toBeVisible()
  await expect(page.getByText('PG', { exact: true })).toHaveCount(0)
})

test('消耗排行：金额 micro→USD、用户链到日志 hours=天数×24；切窗重拉 days', async ({ page }) => {
  await prepare(page)
  const daysQ: string[] = []
  await page.route('**/admin/stats/cashflow?*', (route) =>
    route.fulfill({
      json: {
        today: { recharge_micro: 0, granted_micro: 0, clawed_micro: 0, expired_micro: 0 },
        window: { recharge_micro: 0, granted_micro: 0, clawed_micro: 0, expired_micro: 0 },
      },
    }),
  )
  await page.route('**/admin/leaderboard?*', (route) => {
    daysQ.push(new URL(route.request().url()).searchParams.get('days') ?? '')
    return route.fulfill({
      json: { data: [{ user_id: 7, username: 'alice', requests: 12, tokens: 1500, amount_micro: 1_230_000 }] },
    })
  })

  await page.goto('/admin/revenue')
  await page.getByRole('tab', { name: '用户消耗排行' }).click()
  await expect(page.getByRole('heading', { name: '用户消耗排行' }).first()).toBeVisible()
  await expect(page.getByText('alice')).toBeVisible()
  await expect(page.getByText(/\$1\.23/)).toBeVisible()
  const user = page.getByRole('link', { name: '7', exact: true })
  await expect(user).toHaveAttribute('href', /\/admin\/logs\?.*user_id=7/)
  await expect(user).toHaveAttribute('href', /hours=168/)
  expect(daysQ[0]).toBe('7')
  await page.getByRole('button', { name: '近 30 天' }).click()
  await expect.poll(() => daysQ.at(-1)).toBe('30')
  await expect(user).toHaveAttribute('href', /hours=720/)
})

test('日志行内退款：仅成功扣费行、原因 trim、确认后 POST', async ({ page }) => {
  await prepare(page)
  const refunds: Json[] = []
  const billed = { ...adminLogRow(), log_type: 2, is_error: false, amount_micro: 1_000_000, username: 'alice', request_id: 'req-bill' }
  await page.route(/\/admin\/logs(\/stat)?\?/, (route) => {
    const url = new URL(route.request().url())
    if (url.pathname.endsWith('/stat')) {
      return route.fulfill({
        json: {
          requests: 1, errors: 0, error_rate_bp: 0, tokens: 10, amount_micro: 1_000_000, discount_micro: 0,
          users: 1, cached_tokens: 0, cache_hit_bp: 0, rpm: 1, tpm: 10, rate_source: 'clickhouse',
        },
      })
    }
    return route.fulfill({ json: { data: [billed, { ...adminLogRow(), request_id: 'req-fail', is_error: true, error_code: 'upstream_error', amount_micro: 0 }] } })
  })
  await page.route('**/admin/billing/refund', async (route) => {
    refunds.push(route.request().postDataJSON() as Json)
    await route.fulfill({ json: { outcome: 'refunded', refunded_micro: 1_000_000, balance_after_micro: 5_000_000 } })
  })

  await page.goto('/admin/logs')
  await page.getByRole('row').filter({ hasText: 'upstream_error' }).click()
  await expect(page.getByRole('button', { name: '退款', exact: true })).toHaveCount(0)
  await page.getByRole('row').filter({ hasText: '成功' }).click()
  await expect(page.getByRole('button', { name: '退款', exact: true })).toBeVisible()
  await page.getByRole('button', { name: '退款', exact: true }).click()
  await page.getByPlaceholder('原因').fill('  误扣  ')
  await page.getByRole('button', { name: '退款', exact: true }).click()
  const confirm = page.getByRole('alertdialog')
  await expect(confirm).toContainText('alice')
  await confirm.getByRole('button', { name: '退款', exact: true }).click()
  await expect.poll(() => refunds).toEqual([{ request_id: 'req-bill', reason: '误扣' }])
  await expect(page.getByText(/已退款/)).toBeVisible()
})

test('邮箱登录：totp_required 后才带验证码；首次请求不含 totp_code', async ({ page }) => {
  await prepare(page, { signedIn: false })
  const logins: Json[] = []
  await page.route('**/auth/login', async (route) => {
    logins.push(route.request().postDataJSON() as Json)
    const body = route.request().postDataJSON() as Json
    if (!body.totp_code) await route.fulfill(apiError(401, 'totp_required'))
    else await route.fulfill({ json: { ok: true } })
  })
  await page.route('**/auth/keys', async (route) => {
    await route.fulfill({ json: { api_key: 'sk-okapi-web', key_id: 3 } })
  })

  await page.goto('/')
  await page.locator('#email').fill('  root@ok.test  ')
  await page.locator('#password').fill('secret-password')
  await page.getByRole('button', { name: '登录', exact: true }).click()
  await expect(page.getByText('请输入两步验证码')).toBeVisible()
  await expect(page.locator('#totp')).toBeVisible()
  expect(logins[0]).toEqual({ email: 'root@ok.test', password: 'secret-password' })
  await page.locator('#totp').fill('123456')
  await page.getByRole('button', { name: '登录', exact: true }).click()
  await expect.poll(() => logins.at(-1)).toEqual({
    email: 'root@ok.test',
    password: 'secret-password',
    totp_code: '123456',
  })
  await expect(page).toHaveURL(/\/portal/)
})

function ledgerEvent(id: number, extra: Partial<{
  event_type: string
  delta_micro: number
  source: string
  tags: string[]
  request_id: string | null
  pool: 0 | 1
  balance_after_micro: number | null
}> = {}) {
  return {
    event_id: id,
    event_type: extra.event_type ?? 'recharge',
    delta_micro: extra.delta_micro ?? 1_230_000,
    balance_after_micro: extra.balance_after_micro === undefined ? 10_000_000 : extra.balance_after_micro,
    pool: extra.pool ?? 0,
    source: extra.source ?? 'payment',
    tags: extra.tags ?? ['recharge'],
    request_id: extra.request_id === undefined ? null : extra.request_id,
    created_at: '2026-09-08T12:00:00Z',
  }
}

test('账户流水：进账 micro→USD、标签与退款深链、加载更多 before；订单原币文本、空态与 500', async ({ page }) => {
  await prepare(page)
  const ledgerQs: string[] = []
  const orderQs: string[] = []
  let ledgerPage1 = {
    data: [
      ...Array.from({ length: 48 }, (_, i) => ledgerEvent(50 - i)),
      ledgerEvent(2, {
        event_type: 'refund',
        delta_micro: -1_000_000,
        source: 'admin',
        tags: ['admin_refund'],
        request_id: 'req-bill-xx',
        balance_after_micro: 9_000_000,
      }),
      ledgerEvent(1, {
        event_type: 'sub_grant',
        delta_micro: 5_000_000,
        source: 'admin',
        tags: ['goodwill'],
        pool: 1,
        balance_after_micro: 5_000_000,
      }),
    ],
    next_before: 1,
  }
  await page.route('**/api/me/ledger?*', (route) => {
    const url = new URL(route.request().url())
    ledgerQs.push(url.search)
    if (url.searchParams.get('before') === '1') {
      return route.fulfill({ json: { data: [ledgerEvent(0, { source: 'redeem', tags: ['redeem'] })], next_before: null } })
    }
    return route.fulfill({ json: ledgerPage1 })
  })
  await page.route('**/api/me/orders?*', (route) => {
    const url = new URL(route.request().url())
    orderQs.push(url.search)
    if (url.searchParams.get('before') === '9') {
      return route.fulfill({ json: { data: [{
        id: 9, order_no: 'ord-older', amount_micro: 100_000, currency: 'USD', pay_amount: '0.10',
        gateway: 'epay', status: 2, paid_at: null, created_at: '2026-09-01T00:00:00Z',
      }], next_before: null } })
    }
    return route.fulfill({
      json: {
        data: Array.from({ length: 50 }, (_, i) => ({
          id: 50 - i,
          order_no: i === 0 ? 'ord-paid' : `ord-${50 - i}`,
          amount_micro: 12_340_000,
          currency: 'CNY',
          pay_amount: i === 0 ? '88.00' : null,
          gateway: 'epay',
          status: i === 0 ? 1 : 0,
          paid_at: i === 0 ? '2026-09-08T12:01:00Z' : null,
          created_at: '2026-09-08T12:00:00Z',
        })),
        next_before: 9,
      },
    })
  })

  await page.goto('/portal/ledger')
  await expect(page.locator('#main-content').getByRole('heading', { name: '账户流水' })).toBeVisible()
  await expect.poll(() => ledgerQs[0]).toContain('limit=50')
  expect(ledgerQs[0]).not.toContain('before=')
  await expect(page.getByText(/\+.*\$1\.23/).first()).toBeVisible()
  await expect(page.getByText('充值到账').first()).toBeVisible()
  await expect(page.getByText('退款 · 按日志退款')).toBeVisible()
  const refundLink = page.getByRole('link', { name: /req-bill/ })
  await expect(refundLink).toHaveAttribute('href', '/portal/logs')
  await expect(page.getByText('订阅激活 · 赠送')).toBeVisible()
  await expect(page.getByText('订阅池')).toBeVisible()

  await page.getByRole('button', { name: '加载更多' }).click()
  await expect.poll(() => ledgerQs.some((q) => q.includes('before=1'))).toBe(true)
  await expect(page.getByText('兑换码').first()).toBeVisible()

  await page.getByRole('tab', { name: '充值订单' }).click()
  await expect.poll(() => orderQs[0]).toContain('limit=50')
  await expect(page.getByText('ord-paid')).toBeVisible()
  await page.context().grantPermissions(['clipboard-read', 'clipboard-write'])
  await page.getByRole('row').filter({ hasText: 'ord-paid' }).getByRole('button', { name: '复制', exact: true }).click()
  await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toBe('ord-paid')
  await expect(page.getByText(/\$12\.34/).first()).toBeVisible()
  await expect(page.getByText('88.00 CNY')).toBeVisible()
  await expect(page.getByText('已支付')).toBeVisible()
  await expect(page.getByText('待支付').first()).toBeVisible()
  await page.getByRole('button', { name: '加载更多' }).click()
  await expect.poll(() => orderQs.some((q) => q.includes('before=9'))).toBe(true)
  await expect(page.getByText('失败')).toBeVisible()

  await page.route('**/api/me/orders?*', (route) => route.fulfill({ json: { data: [], next_before: null } }))
  await page.getByRole('tab', { name: '余额变动' }).click()
  await page.getByRole('tab', { name: '充值订单' }).click()
  await expect(page.getByText('还没有充值订单。')).toBeVisible()

  await page.route('**/api/me/ledger?*', (route) => route.fulfill({ json: { data: [], next_before: null } }))
  await page.getByRole('tab', { name: '余额变动' }).click()
  await expect(page.getByText('还没有余额变动记录。充值或兑换后会出现在这里。')).toBeVisible()

  await page.route('**/api/me/ledger?*', (route) => route.fulfill(apiError(500, 'internal_error')))
  await page.reload()
  await page.getByRole('tab', { name: '余额变动' }).click()
  await expect(page.getByRole('alert')).toContainText('服务内部错误')

  await page.route('**/api/me/orders?*', (route) => route.fulfill(apiError(500, 'internal_error')))
  await page.getByRole('tab', { name: '充值订单' }).click()
  await expect(page.getByRole('alert')).toContainText('服务内部错误')
})

test('首启向导：needs_setup 才出现；用户名 trim 后 POST；Key 只展示一次后进控制台', async ({ page }) => {
  await prepare(page, { signedIn: false })
  const bodies: Json[] = []
  await page.route('**/api/setup/status', (route) => route.fulfill({ json: { needs_setup: true } }))
  await page.route('**/api/setup', async (route) => {
    expect(route.request().method()).toBe('POST')
    bodies.push(route.request().postDataJSON() as Json)
    await route.fulfill({ json: { api_key: 'sk-okapi-root-once' } })
  })

  await page.goto('/')
  await expect(page.getByRole('heading', { name: '初始化 Okapi' })).toBeVisible()
  await expect(page.getByRole('button', { name: '邮箱登录' })).toHaveCount(0)
  const create = page.getByRole('button', { name: '创建' })
  await expect(create).toBeDisabled()
  await page.locator('#username').fill('   ')
  await expect(create).toBeDisabled()
  await page.locator('#username').fill('  root  ')
  await create.click()
  await expect.poll(() => bodies).toEqual([{ username: 'root' }])
  await expect(page.getByText('请立即保存此 Key，仅显示一次')).toBeVisible()
  await expect(page.getByText('sk-okapi-root-once')).toBeVisible()
  await page.context().grantPermissions(['clipboard-read', 'clipboard-write'])
  await page.getByRole('button', { name: '复制', exact: true }).click()
  await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toBe('sk-okapi-root-once')
  await page.getByRole('button', { name: '进入控制台' }).click()
  await expect(page).toHaveURL(/\/admin/)
})

test('服务质量：渠道/模型/错误/客户端查询带 days+limit；深链 hours=天数×24；高错误率带 errors_only', async ({ page }) => {
  await prepare(page)
  const qs: { path: string; search: string }[] = []
  await page.route('**/admin/stats/channels?*', (route) => {
    qs.push({ path: '/admin/stats/channels', search: new URL(route.request().url()).search })
    return route.fulfill({
      json: {
        data: [
          {
            channel_id: 42, name: 'openai-main', provider: 'openai', requests: 100, errors: 25,
            error_rate_bp: 2500, ttft_p50_ms: 80, ttft_p95_ms: 200, ttft_p99_ms: 400,
            failovers: 3, sticky_rate_bp: 9000, tokens_per_1k_sec: 12_000, amount_micro: 1_230_000,
          },
          {
            channel_id: 7, name: 'spare', provider: 'openai', requests: 10, errors: 0,
            error_rate_bp: 50, ttft_p50_ms: 70, ttft_p95_ms: 90, ttft_p99_ms: 110,
            failovers: 0, sticky_rate_bp: 10000, tokens_per_1k_sec: 8_000, amount_micro: 100_000,
          },
        ],
      },
    })
  })
  await page.route('**/admin/stats/models?*', (route) => {
    qs.push({ path: '/admin/stats/models', search: new URL(route.request().url()).search })
    return route.fulfill({
      json: {
        data: [{
          model: 'gpt-5', requests: 40, tokens: 9000, amount_micro: 500_000,
          ttft_p50_ms: 80, ttft_p95_ms: 120, ttft_p99_ms: 180,
          latency_p50_ms: 400, latency_p95_ms: 600, latency_p99_ms: 900, tokens_per_1k_sec: 15_000,
        }],
      },
    })
  })
  await page.route('**/admin/stats/errors?*', (route) => {
    qs.push({ path: '/admin/stats/errors', search: new URL(route.request().url()).search })
    return route.fulfill({
      json: {
        total: 12,
        data: [
          {
            error_code: 'upstream_error', errors: 9, share_bp: 7500, upstream_status: 502,
            top_channel_id: 42, top_channel_name: 'openai-main', top_model: 'gpt-5',
          },
          {
            error_code: '', errors: 3, share_bp: 2500, upstream_status: 0,
            top_channel_id: 7, top_channel_name: '', top_model: '',
          },
        ],
      },
    })
  })
  await page.route('**/admin/stats/clients?*', (route) => {
    qs.push({ path: '/admin/stats/clients', search: new URL(route.request().url()).search })
    return route.fulfill({
      json: {
        total_requests: 100,
        data: [
          { client_type: 'sdk', requests: 80, share_bp: 8000, tokens: 1000, amount_micro: 800_000, errors: 1, error_rate_bp: 125, users: 12 },
          { client_type: '', requests: 20, share_bp: 2000, tokens: 100, amount_micro: 50_000, errors: 0, error_rate_bp: 0, users: 2 },
        ],
      },
    })
  })

  await page.goto('/admin/quality')
  await expect(page.locator('#main-content').getByRole('heading', { name: '服务质量' })).toBeVisible()

  await page.getByRole('tab', { name: '渠道健康' }).click()
  await expect.poll(() => qs.some((q) => q.path === '/admin/stats/channels' && q.search.includes('days=7') && q.search.includes('limit=50'))).toBe(true)
  const bad = page.getByRole('link', { name: 'openai-main' })
  await expect(bad).toHaveAttribute('href', /channel_id=42/)
  await expect(bad).toHaveAttribute('href', /hours=168/)
  await expect(bad).toHaveAttribute('href', /errors_only=true/)
  const ok = page.getByRole('link', { name: 'spare' })
  await expect(ok).toHaveAttribute('href', /channel_id=7/)
  await expect(ok).toHaveAttribute('href', /hours=168/)
  await expect(ok).not.toHaveAttribute('href', /errors_only/)
  await expect(page.getByText(/\$1\.23/)).toBeVisible()

  await page.getByRole('tab', { name: '模型时延与吞吐' }).click()
  await expect.poll(() => qs.some((q) => q.path === '/admin/stats/models' && q.search.includes('days=7') && q.search.includes('limit=50'))).toBe(true)
  const model = page.getByRole('link', { name: 'gpt-5' })
  await expect(model).toHaveAttribute('href', /model=gpt-5/)
  await expect(model).toHaveAttribute('href', /hours=168/)

  await page.getByRole('tab', { name: '错误分布' }).click()
  await expect.poll(() => qs.some((q) => q.path === '/admin/stats/errors' && q.search.includes('days=7') && q.search.includes('limit=20'))).toBe(true)
  const err = page.getByRole('link', { name: 'upstream_error' })
  await expect(err).toHaveAttribute('href', /error_code=upstream_error/)
  await expect(err).toHaveAttribute('href', /hours=168/)
  await expect(page.getByText('502')).toBeVisible()
  await expect(page.getByText(/75\.0%/).or(page.getByText('75%'))).toBeVisible()
  await expect(page.getByText('openai-main')).toBeVisible()
  await expect(page.getByText('gpt-5')).toBeVisible()
  const emptyCode = page.getByRole('link', { name: '(empty)' })
  await expect(emptyCode).toHaveAttribute('href', /errors_only=true/)
  await expect(emptyCode).toHaveAttribute('href', /hours=168/)
  await expect(emptyCode).not.toHaveAttribute('href', /error_code=/)
  const blank = page.getByRole('row').filter({ hasText: '(empty)' })
  await expect(blank.getByText('—').first()).toBeVisible()
  await expect(blank.getByText('#7')).toBeVisible()

  await page.route('**/admin/stats/errors?*', (route) =>
    route.fulfill({ json: { total: 0, data: [] } }),
  )
  await page.getByRole('tab', { name: '客户端分布' }).click()
  await page.getByRole('tab', { name: '错误分布' }).click()
  await expect(page.getByText('窗口内没有失败请求。')).toBeVisible()

  await page.getByRole('tab', { name: '客户端分布' }).click()
  await expect.poll(() => qs.some((q) => q.path === '/admin/stats/clients' && q.search.includes('days=7') && q.search.includes('limit=30'))).toBe(true)
  await expect(page.getByText('sdk')).toBeVisible()
  await expect(page.getByText('未识别')).toBeVisible()

  await page.getByRole('button', { name: '近 30 天' }).click()
  await expect.poll(() => qs.some((q) => q.path === '/admin/stats/clients' && q.search.includes('days=30'))).toBe(true)

  await page.route('**/admin/stats/clients?*', (route) =>
    route.fulfill({ json: { total_requests: 0, data: [] } }),
  )
  await page.getByRole('tab', { name: '错误分布' }).click()
  await page.getByRole('tab', { name: '客户端分布' }).click()
  await expect(page.getByText('窗口内还没有调用记录。')).toBeVisible()

  await page.route('**/admin/stats/clients?*', (route) => {
    if (route.request().isNavigationRequest()) return route.fallback()
    return route.fulfill(apiError(500, 'internal_error'))
  })
  await page.route('**/admin/stats/errors?*', (route) => {
    if (route.request().isNavigationRequest()) return route.fallback()
    return route.fulfill(apiError(500, 'internal_error'))
  })
  await page.route('**/admin/stats/channels?*', (route) => {
    if (route.request().isNavigationRequest()) return route.fallback()
    return route.fulfill(apiError(500, 'internal_error'))
  })
  await page.route('**/admin/stats/models?*', (route) => {
    if (route.request().isNavigationRequest()) return route.fallback()
    return route.fulfill(apiError(500, 'internal_error'))
  })
  await page.getByRole('button', { name: '近 1 天' }).click()
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误，请稍后再试' })).toBeVisible()
  await expect(page.getByRole('button', { name: '重试' })).toHaveCount(0)

  await page.getByRole('tab', { name: '错误分布' }).click()
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误，请稍后再试' })).toBeVisible()
  await expect(page.getByRole('button', { name: '重试' })).toHaveCount(0)

  await page.getByRole('tab', { name: '渠道健康' }).click()
  await expect(page.getByText('服务内部错误，请稍后再试')).toBeVisible()
  await expect(page.getByRole('button', { name: '重试' })).toHaveCount(0)

  await page.getByRole('tab', { name: '模型时延与吞吐' }).click()
  await expect(page.getByText('服务内部错误，请稍后再试')).toBeVisible()
  await expect(page.getByRole('button', { name: '重试' })).toHaveCount(0)

  await page.route('**/admin/stats/channels?*', (route) => {
    if (route.request().isNavigationRequest()) return route.fallback()
    return route.fulfill({ json: { data: [] } })
  })
  await page.route('**/admin/stats/models?*', (route) => {
    if (route.request().isNavigationRequest()) return route.fallback()
    return route.fulfill({ json: { data: [] } })
  })
  await page.getByRole('button', { name: '近 30 天' }).click()
  await page.getByRole('tab', { name: '渠道健康' }).click()
  await expect(page.getByRole('link', { name: 'openai-main' })).toHaveCount(0)
  await page.getByRole('tab', { name: '模型时延与吞吐' }).click()
  await expect(page.getByRole('link', { name: 'gpt-5' })).toHaveCount(0)
})

test('站点公告：warning 可按 updated_at 关掉；换版再出；critical 不能关', async ({ page }) => {
  await prepare(page)
  let notice = {
    title: '维护窗口',
    body: '今晚 02:00 起短暂停服',
    level: 'warning' as const,
    updated_at: '2026-09-08T01:00:00Z',
  }
  await page.route('**/api/notice', (route) => route.fulfill({ json: { notice } }))

  await page.goto('/portal')
  const banner = page.getByRole('status').filter({ hasText: '维护窗口' })
  await expect(banner).toBeVisible()
  await expect(banner.getByText('今晚 02:00 起短暂停服')).toBeVisible()
  await banner.getByRole('button', { name: '关闭' }).click()
  await expect(banner).toHaveCount(0)

  await page.reload()
  await expect(page.getByText('维护窗口')).toHaveCount(0)

  notice = { ...notice, updated_at: '2026-09-08T03:00:00Z', title: '维护改期', body: '改到明天' }
  await page.reload()
  await expect(page.getByRole('status').filter({ hasText: '维护改期' })).toBeVisible()

  notice = { title: '紧急停服', body: '立刻停止调用', level: 'critical', updated_at: '2026-09-08T04:00:00Z' }
  await page.reload()
  const alert = page.getByRole('alert').filter({ hasText: '紧急停服' })
  await expect(alert).toBeVisible()
  await expect(alert.getByRole('button', { name: '关闭' })).toHaveCount(0)
})

test('API Key 登录：首尾空白 trim 后作为 Bearer 探活 /api/me', async ({ page }) => {
  await prepare(page, { signedIn: false })
  const auths: string[] = []
  await page.route('**/api/me', (route) => {
    if (new URL(route.request().url()).pathname !== '/api/me') return route.fallback()
    auths.push(route.request().headers().authorization ?? '')
    return route.fulfill({
      json: {
        user_id: 1, key_id: 3, group: 'default', balance_micro: 1_000_000,
        balance_expires_at: null, role: 1, permissions: [],
      },
    })
  })

  await page.goto('/')
  await page.getByRole('button', { name: 'API Key' }).click()
  const submit = page.getByRole('button', { name: '登录', exact: true })
  await expect(submit).toBeDisabled()
  await page.locator('#key').fill('   ')
  await expect(submit).toBeDisabled()
  await page.locator('#key').fill('  sk-okapi-trimmed  ')
  await submit.click()
  await expect.poll(() => auths[0]).toBe('Bearer sk-okapi-trimmed')
  await expect(page).toHaveURL(/\/portal/)
})

function breakdown(scope: string, days: number) {
  return {
    scope,
    days,
    live: { rpm: 12, tpm: 100, rpd: 1, rpm_limit: 60, tpm_limit: 1000, rpd_limit: null },
    wallet_window_spend_micro: 100_000,
    total: {
      requests: 10, prompt_tokens: 100, cached_tokens: 0, completion_tokens: 20, reasoning_tokens: 0,
      tokens: 120, amount_micro: 500_000, discount_micro: 50_000, cache_hit_bp: 0,
      avg_rpm_micro: 1_000_000, avg_tpm_micro: 2_000_000, success_rate_bp: 9000,
      avg_latency_ms: 100, avg_ttft_ms: 50, tokens_per_1k_sec: 1000,
    },
    data: [],
  }
}

test('门户总览：scope/days 进查询；key 视角本分钟 RPM；全账户退回平均 TPM；订阅剩余与到期', async ({ page }) => {
  await prepare(page)
  const qs: URLSearchParams[] = []
  await page.route('**/api/me', (route) => {
    if (new URL(route.request().url()).pathname !== '/api/me') return route.fallback()
    return route.fulfill({
      json: {
        user_id: 1, key_id: 1, group: 'default', balance_micro: 10_000_000,
        balance_expires_at: '2026-09-15T00:00:00Z', role: 100, permissions: ['*'],
        subscription_remaining_micro: 2_000_000, subscription_until_unix: 1_893_456_000,
      },
    })
  })
  await page.route('**/api/me/stats/breakdown?*', (route) => {
    const p = new URL(route.request().url()).searchParams
    qs.push(p)
    return route.fulfill({ json: breakdown(p.get('scope') ?? 'key', Number(p.get('days') ?? 7)) })
  })

  await page.goto('/portal')
  await expect.poll(() => qs[0]?.get('scope')).toBe('key')
  expect(qs[0]?.get('days')).toBe('7')
  await expect(page.getByText('本分钟 RPM')).toBeVisible()
  await expect(page.getByText('12 / 60')).toBeVisible()
  await expect(page.getByRole('link', { name: /订阅剩余/ })).toHaveAttribute('href', '/portal/plans')
  await expect(page.getByText(/\$2\.00/)).toBeVisible()
  await expect(page.getByText(/到期清零/)).toBeVisible()

  await page.getByRole('button', { name: '全账户' }).click()
  await expect.poll(() => qs.some((p) => p.get('scope') === 'user' && p.get('days') === '7')).toBe(true)
  await expect(page.getByText('平均 TPM')).toBeVisible()
  await page.getByRole('button', { name: '近 90 天' }).click()
  await expect.poll(() => qs.some((p) => p.get('scope') === 'user' && p.get('days') === '90')).toBe(true)
  await expect(page.getByRole('region', { name: '模型消费排行', exact: true }).getByText('当前范围和时段内还没有调用。发起请求后，等待用量入库即可查看。')).toBeVisible()

  await page.route('**/api/me/stats/breakdown?*', (route) => route.fulfill(apiError(500, 'internal_error')))
  await page.reload()
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误，请稍后再试' })).toBeVisible()
  await expect(page.getByRole('button', { name: '重试' })).toBeVisible()
})

test('站点规模条：自动停用与未定价文案、深链到用户/密钥/渠道/定价', async ({ page }) => {
  await prepare(page)
  await page.route('**/admin/diagnose', (route) =>
    route.fulfill({
      json: {
        postgres: true, redis: true, clickhouse: true, nats_connected: true,
        outbox_pending: 0, dlq_depth: 0, cooling_keys: 0, pricebook_epoch: 1,
      },
    }),
  )
  await page.route('**/admin/stats/realtime*', (route) =>
    route.fulfill({
      json: { window_secs: 60, qps_milli: 0, requests: 0, errors: 0, error_rate_bp: 0, tokens: 0, amount_micro: 0, series: [] },
    }),
  )
  await page.route('**/admin/stats/inventory', (route) =>
    route.fulfill({
      json: {
        users: { total: 12, active: 8, new_today: 2, new_7d: 5 },
        api_keys: { total: 20, active: 9, used_7d: 4 },
        channels: { total: 6, healthy: 3, no_key: 1, auto_disabled: 2, disabled: 0 },
        channel_keys: { active: 3, cooling: 0, rate_limited: 0, quota_exhausted: 0, banned: 0, invalid: 0 },
        models: { total: 10, priced: 7, served: 5 },
        groups: 2,
      },
    }),
  )

  await page.goto('/admin')
  const strip = page.getByRole('region', { name: '站点速览', exact: true })
  await expect(strip.getByRole('link', { name: /今日 \+2/ })).toHaveAttribute('href', '/admin/users')
  await expect(strip.getByRole('link', { name: /4 把近 7 天用过/ })).toHaveAttribute('href', '/admin/keys')
  await expect(strip.getByRole('link', { name: /2 条已自动停用/ })).toHaveAttribute('href', '/admin/channels')
  await expect(strip.getByRole('link', { name: /3 个未定价/ })).toHaveAttribute('href', '/admin/pricing')

  await page.route('**/admin/stats/inventory', (route) =>
    route.fulfill({
      json: {
        users: { total: 12, active: 8, new_today: 0, new_7d: 5 },
        api_keys: { total: 20, active: 9, used_7d: 4 },
        channels: { total: 6, healthy: 3, no_key: 1, auto_disabled: 0, disabled: 0 },
        channel_keys: { active: 3, cooling: 0, rate_limited: 0, quota_exhausted: 0, banned: 0, invalid: 0 },
        models: { total: 10, priced: 10, served: 5 },
        groups: 2,
      },
    }),
  )
  await page.reload()
  await expect(strip.getByRole('link', { name: /近 7 天 \+5/ })).toHaveAttribute('href', '/admin/users')
  await expect(strip.getByRole('link', { name: /1 条启用但无可用 key/ })).toHaveAttribute('href', '/admin/channels')
  await expect(strip.getByRole('link', { name: /5 个有启用渠道/ })).toHaveAttribute('href', '/admin/pricing')

  await page.route('**/admin/stats/inventory', (route) => {
    if (route.request().isNavigationRequest()) return route.fallback()
    return route.fulfill(apiError(500, 'internal_error'))
  })
  await page.reload()
  await expect(page.getByRole('heading', { name: '站点规模' })).toHaveCount(0)
  await expect(strip.getByRole('alert').filter({ hasText: '服务内部错误，请稍后再试' })).toBeVisible()
})

test('用户用量抽屉：usage?days=7、近 7 天消费 micro→USD、流水操作者、日志深链 hours=168', async ({ page }) => {
  await prepare(page)
  const usageQ: string[] = []
  await page.route('**/admin/users?*', (route) => {
    if (route.request().method() !== 'GET') return route.fallback()
    return route.fulfill({
      json: {
        total: 1,
        data: [{ id: 7, username: 'alice', email: 'alice@ok.test', role: 1, status: 1, balance_micro: 5_000_000, admin_role_id: null, price_multiplier: '1' }],
      },
    })
  })
  await page.route('**/admin/users/7/overview', (route) =>
    route.fulfill({
      json: {
        user: { id: 7, username: 'alice', role: 1, status: 1, balance_micro: 5_000_000, price_multiplier: '1' },
        groups: [{ code: 'vip', priority: 1 }],
        keys: [{ id: 3 }, { id: 4 }],
      },
    }),
  )
  await page.route('**/admin/users/7/usage?*', (route) => {
    usageQ.push(new URL(route.request().url()).search)
    return route.fulfill({
      json: {
        days: 7,
        stats_available: true,
        daily: [{ day: '2026-09-08', requests: 4, amount_micro: 1_230_000 }],
        by_model: [{ model: 'gpt-5', requests: 4, amount_micro: 1_230_000, tokens: 900 }],
        ledger: [{
          event_id: 11, event_type: 'adjust', delta_micro: 1_000_000, balance_after_micro: 6_000_000,
          actor: 'root', tags: ['compensation'], reason: 'ticket 9', created_at: '2026-09-08T12:00:00Z',
        }],
      },
    })
  })

  await page.goto('/admin/users')
  await page.getByRole('row').filter({ hasText: 'alice' }).getByRole('button', { name: '管理', exact: true }).click()
  const drawer = await openedDialog(page)
  await expect(drawer.getByRole('heading', { name: '用户 #7' })).toBeVisible()
  await expect.poll(() => usageQ[0]).toContain('days=7')
  await expect(drawer.getByText(/近 7 天消费.*\$1\.23/)).toBeVisible()
  await expect(drawer.getByText(/gpt-5/)).toBeVisible()
  await expect(drawer.getByText('额度调整')).toBeVisible()
  await expect(drawer.getByText('补偿')).toBeVisible()
  await expect(drawer.getByText('“ticket 9”')).toBeVisible()
  await expect(drawer.getByText('root')).toBeVisible()
  await expect(drawer.getByRole('link', { name: '查看近 7 天调用明细' })).toHaveAttribute('href', /user_id=7/)
  await expect(drawer.getByRole('link', { name: '查看近 7 天调用明细' })).toHaveAttribute('href', /hours=168/)
  await expect(drawer.getByText(/密钥 2/)).toBeVisible()
  await expect(drawer.getByText(/\$5\.00/)).toBeVisible()

  await page.route('**/admin/users/7/usage?*', (route) =>
    route.fulfill({ json: { days: 7, stats_available: true, daily: [], by_model: [], ledger: [] } }),
  )
  await page.reload()
  await page.getByRole('row').filter({ hasText: 'alice' }).getByRole('button', { name: '管理', exact: true }).click()
  const empty = await openedDialog(page)
  await expect(empty.getByText('最近余额变动')).toBeVisible()
  await expect(empty.getByText('暂无数据').first()).toBeVisible()

  await page.route('**/admin/users/7/overview', (route) => route.fulfill(apiError(500, 'internal_error')))
  await page.route('**/admin/users/7/usage?*', (route) => route.fulfill(apiError(500, 'internal_error')))
  await page.reload()
  await page.getByRole('row').filter({ hasText: 'alice' }).getByRole('button', { name: '管理', exact: true }).click()
  const failed = await openedDialog(page)
  await expect(failed.getByRole('alert').filter({ hasText: '服务内部错误' })).toHaveCount(2)
})

test('登出：POST /auth/logout 空体后清 key 回到登录页', async ({ page }) => {
  await prepare(page)
  const logouts: Json[] = []
  await page.route('**/auth/logout', async (route) => {
    expect(route.request().method()).toBe('POST')
    logouts.push(route.request().postDataJSON() as Json)
    await route.fulfill({ json: { ok: true } })
  })

  await page.goto('/portal')
  await page.getByRole('button', { name: '退出登录' }).click()
  await expect.poll(() => logouts).toEqual([{}])
  await expect(page).toHaveURL(/\/$|\/\?/)
  await expect(page.getByRole('button', { name: '登录', exact: true })).toBeVisible()
})

test('OAuth 着陆：?oauth=done 兑 key 只带 name=oauth，然后进门户', async ({ page }) => {
  await prepare(page, { signedIn: false })
  const keys: Json[] = []
  await page.route('**/auth/keys', async (route) => {
    keys.push(route.request().postDataJSON() as Json)
    await route.fulfill({ json: { api_key: 'sk-okapi-oauth', key_id: 8 } })
  })

  await page.goto('/?oauth=done')
  await expect.poll(() => keys).toEqual([{ name: 'oauth' }])
  await expect(page).toHaveURL(/\/portal/)
})

test('用户/密钥列表行内用量：entity-usage kind/ids/days=7；501 显示 — 不伪装成零', async ({ page }) => {
  await prepare(page)
  const usageQ: URLSearchParams[] = []
  let usageFail = false
  await page.route('**/admin/users?*', (route) => {
    if (route.request().method() !== 'GET') return route.fallback()
    return route.fulfill({
      json: {
        total: 1,
        data: [{
          id: 7, username: 'alice', email: 'alice@ok.test', role: 1, status: 1,
          balance_micro: 5_000_000, admin_role_id: null, price_multiplier: '1',
        }],
      },
    })
  })
  await page.route('**/admin/keys?*', (route) => {
    if (route.request().method() !== 'GET') return route.fallback()
    return route.fulfill({
      json: {
        total: 1,
        data: [{
          id: 9, user_id: 7, username: 'alice', team_id: null, name: 'ci-bot',
          key_prefix: 'sk-okapi-abcd', status: 1, quota_mode: 0, quota_micro: null,
          used_micro: 500_000, model_allowlist: null, group_override: null,
          ip_allowlist: null, rpm_limit: null, max_concurrency: null, expires_at: null,
          last_used_at: null, created_at: '2026-01-01T00:00:00Z',
        }],
      },
    })
  })
  await page.route('**/admin/stats/entity-usage*', (route) => {
    const p = new URL(route.request().url()).searchParams
    usageQ.push(p)
    if (usageFail) return route.fulfill(apiError(501, 'clickhouse_unavailable'))
    const kind = p.get('kind')
    const id = kind === 'api_key' ? '9' : '7'
    return route.fulfill({
      json: {
        days: 7,
        data: { [id]: { today_micro: 1_230_000, window_micro: 5_000_000, requests: 4, last_day: null } },
      },
    })
  })

  await page.goto('/admin/users')
  await expect.poll(() => usageQ.some((p) => p.get('kind') === 'user' && p.get('ids') === '7' && p.get('days') === '7')).toBe(true)
  const userUsage = page.getByRole('link', { name: /今日/ })
  await expect(userUsage).toContainText(/\$1\.23/)
  await expect(userUsage).toContainText(/7 天/)
  await expect(userUsage).toHaveAttribute('href', /\/admin\/stats/)
  await expect(userUsage).toHaveAttribute('href', /user_id=7/)

  await page.goto('/admin/keys')
  await expect.poll(() => usageQ.some((p) => p.get('kind') === 'api_key' && p.get('ids') === '9' && p.get('days') === '7')).toBe(true)
  const keyUsage = page.getByRole('link', { name: /今日/ })
  await expect(keyUsage).toContainText(/\$1\.23/)
  await expect(keyUsage).toHaveAttribute('href', /api_key_id=9/)

  usageFail = true
  await page.reload()
  await expect(page.getByText('—').first()).toBeVisible()
  await expect(page.getByRole('link', { name: /今日/ })).toHaveCount(0)
})

test('模型定价：仅看未定价进 URL 与查询；搜索 q 一并带上', async ({ page }) => {
  await prepare(page)
  const qs: URLSearchParams[] = []
  const unpriced = {
    model_name: 'gpt-5', vendor: 'openai', status: 1, pricing_mode: null,
    model_ratio: null, completion_ratio: null, cache_ratio: null, cache_write_ratio: null,
    tier_expr: null, audio_ratio: null, audio_completion_ratio: null, image_ratio: null,
    per_call_price_micro: null, fallback_models: [],
  }
  const priced = {
    model_name: 'o1', vendor: 'openai', status: 1, pricing_mode: 'ratio',
    model_ratio: '1.250000', completion_ratio: '8.000000', cache_ratio: '0.100000',
    cache_write_ratio: '1.250000', tier_expr: null, audio_ratio: '2',
    audio_completion_ratio: '1.5', image_ratio: '1.5',
    per_call_price_micro: null, fallback_models: [],
  }
  await page.route('**/admin/models?*', (route) => {
    const p = new URL(route.request().url()).searchParams
    qs.push(p)
    let data = [unpriced, priced]
    if (p.get('unpriced') === 'true') data = data.filter((m) => m.pricing_mode === null)
    const q = p.get('q') ?? ''
    if (q) data = data.filter((m) => m.model_name.includes(q))
    return route.fulfill({
      json: { data, total: data.length, unpriced: data.filter((m) => m.pricing_mode === null).length },
    })
  })
  await page.route('**/admin/channels', (route) => {
    if (route.request().isNavigationRequest() || route.request().method() !== 'GET') return route.fallback()
    return route.fulfill({ json: { data: [{ id: 1, status: 1, models: ['gpt-5'] }] } })
  })

  await page.goto('/admin/pricing')
  await expect(page.getByText('gpt-5')).toBeVisible()
  await expect(page.getByText('未定价', { exact: true })).toBeVisible()
  const o1 = page.getByRole('row').filter({ hasText: 'o1' })
  await expect(o1.getByText('ratio')).toBeVisible()
  await expect(o1.getByText('1.25').first()).toBeVisible()
  await expect(o1.getByText('8')).toBeVisible()
  await expect(o1.getByText('$2.50 / $20.00')).toBeVisible()
  await expect(o1.getByText('0.1')).toBeVisible()
  await expect(o1.getByText('1.25').nth(1)).toBeVisible()
  await expect(o1.getByText('2 ×1.5')).toBeVisible()
  await expect(o1.getByText('1.5').last()).toBeVisible()
  await expect(o1.getByText('无渠道')).toBeVisible()
  await expect(page.getByRole('row').filter({ hasText: 'gpt-5' }).getByRole('link', { name: '1 条渠道' })).toHaveAttribute('href', '/admin/channels')
  await page.getByRole('button', { name: /仅看未定价/ }).click()
  await expect(page).toHaveURL(/unpriced=true/)
  await expect.poll(() => qs.some((p) => p.get('unpriced') === 'true')).toBe(true)
  await expect(page.getByRole('button', { name: /仅看未定价/ })).toHaveAttribute('aria-pressed', 'true')
  await expect(page.getByRole('row').filter({ hasText: 'o1' })).toHaveCount(0)

  await page.locator('#m-search').fill('gpt-5')
  await page.getByRole('button', { name: '搜索', exact: true }).click()
  await expect(page).toHaveURL(/q=gpt-5/)
  await expect.poll(() => qs.some((p) => p.get('q') === 'gpt-5' && p.get('unpriced') === 'true')).toBe(true)

  await page.getByRole('button', { name: /仅看未定价/ }).click()
  await expect(page).not.toHaveURL(/unpriced=/)
  await expect.poll(() => qs.at(-1)?.get('unpriced')).toBeNull()

  await page.locator('#m-search').fill('nope')
  await page.getByRole('button', { name: '搜索', exact: true }).click()
  await expect(page.getByText('没有匹配的结果')).toBeVisible()
  await page.getByRole('button', { name: '清空筛选' }).click()
  await expect(page).not.toHaveURL(/q=/)
  await expect(page.getByText('o1')).toBeVisible()

  await page.route('**/admin/models?*', (route) =>
    route.fulfill({ json: { data: [], total: 0, unpriced: 0 } }),
  )
  await page.reload()
  await expect(page.getByText('还没有模型，可新增或从 new-api 导入。')).toBeVisible()

  await page.route('**/admin/models?*', (route) => {
    if (route.request().isNavigationRequest() || route.request().method() !== 'GET') return route.fallback()
    return route.fulfill(apiError(500, 'internal_error'))
  })
  await page.reload()
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误，请稍后再试' })).toBeVisible()
  await expect(page.getByRole('button', { name: '重试' })).toBeVisible()
})

test('总览 KPI / 实时条：overview?days= 与 realtime?window=60；切窗重拉', async ({ page }) => {
  await prepare(page)
  const overviewDays: string[] = []
  const realtimeQ: string[] = []
  const trendDays: string[] = []
  const bucket = (n: Json) => ({
    requests: 0, tokens: 0, amount_micro: 0, original_micro: 0, discount_micro: 0,
    upstream_cost_micro: 0, margin_micro: null, margin_rate_bp: null,
    errors: 0, error_rate_bp: 0, active_users: 0, ...n,
  })
  await page.route('**/admin/diagnose', (route) =>
    route.fulfill({
      json: {
        postgres: true, redis: true, clickhouse: true, nats_connected: true,
        outbox_pending: 0, dlq_depth: 0, cooling_keys: 0, pricebook_epoch: 1,
      },
    }),
  )
  await page.route('**/admin/stats/inventory', (route) =>
    route.fulfill({
      json: {
        users: { total: 1, active: 1, new_today: 0, new_7d: 0 },
        api_keys: { total: 1, active: 1, used_7d: 0 },
        channels: { total: 1, healthy: 1, no_key: 0, auto_disabled: 0, disabled: 0 },
        channel_keys: { active: 1, cooling: 0, rate_limited: 0, quota_exhausted: 0, banned: 0, invalid: 0 },
        models: { total: 1, priced: 1, served: 1 },
        groups: 1,
      },
    }),
  )
  await page.route('**/admin/stats/realtime*', (route) => {
    realtimeQ.push(new URL(route.request().url()).search)
    return route.fulfill({
      json: {
        window_secs: 60, qps_milli: 1500, requests: 42, errors: 1, error_rate_bp: 150,
        tokens: 1234, amount_micro: 1_230_000,
        series: [{ ts: 1, requests: 2, tokens: 10, errors: 0, amount_micro: 1000 }],
      },
    })
  })
  await page.route('**/admin/stats/overview*', (route) => {
    overviewDays.push(new URL(route.request().url()).searchParams.get('days') ?? '')
    return route.fulfill({
      json: {
        days: Number(new URL(route.request().url()).searchParams.get('days') ?? 7),
        today: bucket({ requests: 10, amount_micro: 1_230_000, tokens: 1000, active_users: 5, error_rate_bp: 250 }),
        yesterday: bucket({ requests: 8, amount_micro: 1_000_000, tokens: 800, active_users: 4, error_rate_bp: 100 }),
        window: bucket({ requests: 100, amount_micro: 10_000_000, tokens: 9000, active_users: 20, error_rate_bp: 200 }),
      },
    })
  })
  await page.route('**/admin/stats/trend*', (route) => {
    const days = new URL(route.request().url()).searchParams.get('days') ?? '7'
    trendDays.push(days)
    return route.fulfill({
      json: {
        days: Number(days),
        window: {
          start_date: days === '30' ? '2026-08-10' : '2026-09-02',
          end_date: '2026-09-08',
          timezone: 'UTC',
        },
        total: { requests: 42, amount_micro: 1_230_000, tokens: 1234 },
        data: [{ bucket: '2026-09-08', requests: 42, amount_micro: 1_230_000, discount_micro: 0 }],
      },
    })
  })

  await page.goto('/admin')
  await expect.poll(() => overviewDays[0]).toBe('7')
  await expect.poll(() => trendDays[0]).toBe('7')
  await expect.poll(() => realtimeQ.some((s) => s.includes('window=60'))).toBe(true)
  await expect(page.getByText('实时')).toBeVisible()
  await expect(page.getByText('1.5', { exact: true }).first()).toBeVisible()
  await expect(page.getByText('60 秒请求')).toBeVisible()
  await expect(page.getByText('42', { exact: true }).first()).toBeVisible()
  await expect(page.getByText(/\$1\.23/).first()).toBeVisible()
  await expect(page.getByText('昨日').first()).toBeVisible()
  await expect(page.getByText(/近 7 天/).first()).toBeVisible()
  await expect(page.getByRole('heading', { name: '请求量与收入趋势' })).toBeVisible()
  const trend = page.getByRole('group', { name: '请求量与收入趋势' })
  await trend.getByRole('button', { name: '数据表' }).click()
  await expect(trend.getByRole('cell', { name: '42', exact: true })).toBeVisible()

  await page.getByRole('button', { name: '近 30 天' }).click()
  await expect.poll(() => overviewDays.at(-1)).toBe('30')
  await expect.poll(() => trendDays.at(-1)).toBe('30')
  await expect(page.getByText(/近 30 天/).first()).toBeVisible()
  const metric = page.getByLabel('图表指标')
  await metric.getByRole('button', { name: '实际消费' }).click()
  await expect(metric.getByRole('button', { name: '实际消费' })).toHaveAttribute('aria-pressed', 'true')
  await trend.getByRole('button', { name: '数据表' }).click()
  await expect(trend.getByText(/\$1\.23/)).toBeVisible()

  await page.route('**/admin/stats/trend*', (route) => route.fulfill(apiError(500, 'internal_error')))
  await page.reload()
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误，请稍后再试' }).first()).toBeVisible()
  await expect(page.getByRole('button', { name: '重试' }).first()).toBeVisible()

  await page.route('**/admin/stats/overview*', (route) => route.fulfill(apiError(500, 'internal_error')))
  await page.reload()
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误，请稍后再试' }).first()).toBeVisible()
  await expect(page.getByRole('button', { name: '重试' }).first()).toBeVisible()

  await page.route('**/admin/stats/realtime*', (route) => {
    if (route.request().isNavigationRequest()) return route.fallback()
    return route.fulfill(apiError(500, 'internal_error'))
  })
  await page.reload()
  await expect(page.getByRole('region', { name: '实时流量' }).getByRole('alert')).toBeVisible()
})

test('语言与主题菜单：切 English 持久化 okapi.lang；深色挂 class 并写入 okapi.theme', async ({ page }) => {
  await prepare(page)
  await page.goto('/portal')
  await expect(page.locator('#main-content').getByRole('heading', { name: '总览' })).toBeVisible()

  await page.getByRole('button', { name: '主题' }).click()
  await page.getByRole('menuitem', { name: '深色' }).click()
  await expect(page.locator('html')).toHaveClass(/dark/)
  await expect.poll(() => page.evaluate(() => localStorage.getItem('okapi.theme'))).toBe('dark')

  await page.getByRole('button', { name: '语言' }).click()
  await page.getByRole('menuitem', { name: 'English' }).click()
  await expect(page.locator('#main-content').getByRole('heading', { name: 'Dashboard' })).toBeVisible()
  await expect(page.getByRole('button', { name: 'Sign out' })).toBeVisible()
  await expect.poll(() => page.evaluate(() => localStorage.getItem('okapi.lang'))).toBe('en')

  await page.getByRole('button', { name: 'Theme' }).click()
  await page.getByRole('menuitem', { name: 'System' }).click()
  await expect.poll(() => page.evaluate(() => localStorage.getItem('okapi.theme'))).toBeNull()
})

test('消耗排行空态与 500：不伪装成零金额', async ({ page }) => {
  await prepare(page)
  await page.route('**/admin/stats/cashflow?*', (route) =>
    route.fulfill({
      json: {
        today: { recharge_micro: 0, granted_micro: 0, clawed_micro: 0, expired_micro: 0 },
        window: { recharge_micro: 0, granted_micro: 0, clawed_micro: 0, expired_micro: 0 },
      },
    }),
  )
  let leaderboard: 'empty' | 'error' = 'empty'
  await page.route('**/admin/leaderboard?*', (route) => {
    if (leaderboard === 'error') return route.fulfill(apiError(500, 'internal_error'))
    return route.fulfill({ json: { data: [] } })
  })

  await page.goto('/admin/revenue')
  await page.getByRole('tab', { name: '用户消耗排行' }).click()
  const card = page.getByRole('heading', { name: '用户消耗排行' }).first().locator('xpath=../..')
  await expect(card.getByText('暂无数据')).toBeVisible()
  await expect(card.getByText(/\$0/)).toHaveCount(0)

  leaderboard = 'error'
  await page.getByRole('button', { name: '近 30 天' }).click()
  await expect(page.getByRole('alert')).toContainText('服务内部错误')
  await expect(page.getByText(/\$0/)).toHaveCount(0)
})

test('日志行内退款：already_refunded 走幂等文案，不显示已退金额', async ({ page }) => {
  await prepare(page)
  const billed = { ...adminLogRow(), log_type: 2, is_error: false, amount_micro: 1_000_000, username: 'alice', request_id: 'req-dup' }
  await page.route(/\/admin\/logs(\/stat)?\?/, (route) => {
    const url = new URL(route.request().url())
    if (url.pathname.endsWith('/stat')) {
      return route.fulfill({
        json: {
          requests: 1, errors: 0, error_rate_bp: 0, tokens: 10, amount_micro: 1_000_000, discount_micro: 0,
          users: 1, cached_tokens: 0, cache_hit_bp: 0, rpm: 1, tpm: 10, rate_source: 'clickhouse',
        },
      })
    }
    return route.fulfill({ json: { data: [billed] } })
  })
  await page.route('**/admin/billing/refund', (route) =>
    route.fulfill({ json: { outcome: 'already_refunded' } }),
  )

  await page.goto('/admin/logs')
  await page.getByRole('row').filter({ hasText: '成功' }).click()
  await page.getByRole('button', { name: '退款', exact: true }).click()
  await page.getByPlaceholder('原因').fill('重复点')
  await page.getByRole('button', { name: '退款', exact: true }).click()
  await page.getByRole('alertdialog').getByRole('button', { name: '退款', exact: true }).click()
  await expect(page.getByText('这笔此前已退过款，本次未重复入账（幂等保护）')).toBeVisible()
  await expect(page.getByText(/已退款 \$/)).toHaveCount(0)
})

test('用量分析过滤条：深链 user_id 进查询并回填用户名；加模型过滤；点 × 拿掉', async ({ page }) => {
  await prepare(page)
  const qs: URLSearchParams[] = []
  await page.route('**/admin/stats/trend?*', (route) => {
    const p = new URL(route.request().url()).searchParams
    qs.push(p)
    const uid = p.get('user_id')
    return route.fulfill({
      json: {
        days: 7,
        granularity: 'day',
        scope: uid === '7' ? { user: { id: 7, username: 'alice' } } : {},
        total: { requests: 10, amount_micro: 1_230_000, tokens: 100, errors: 0, error_rate_bp: 0, prompt_tokens: 80, completion_tokens: 20, discount_micro: 200_000 },
        previous: { requests: 8, amount_micro: 1_000_000, tokens: 100, error_rate_bp: 0, prompt_tokens: 80 },
        data: [],
      },
    })
  })

  await page.goto('/admin/stats?user_id=7')
  await expect.poll(() => qs.some((p) => p.get('user_id') === '7' && p.get('days') === '7')).toBe(true)
  await expect(page.getByText('alice')).toBeVisible()
  await expect(page.getByText(/\$1\.23/).first()).toBeVisible()
  await expect(page.getByText('▲ +25%')).toBeVisible()
  await expect(page.getByText('▲ +23%')).toBeVisible()
  await expect(page.getByText('持平').first()).toBeVisible()
  await expect(page.getByText('对比上一个 7 天').first()).toBeVisible()
  await expect(page.getByText('输入 80 · 输出 20')).toBeVisible()
  await expect(page.getByText(/含让利.*\$0\.20/)).toBeVisible()

  await page.getByPlaceholder('输入名称，回车添加').fill('gpt-5')
  await page.getByRole('button', { name: '添加过滤' }).click()
  await expect(page).toHaveURL(/user_id=7/)
  await expect(page).toHaveURL(/model=gpt-5/)
  await expect.poll(() => qs.some((p) => p.get('user_id') === '7' && p.get('model') === 'gpt-5')).toBe(true)

  await page.getByRole('button', { name: '移除过滤 alice' }).click()
  await expect(page).not.toHaveURL(/user_id=/)
  await expect(page).toHaveURL(/model=gpt-5/)
  await expect.poll(() => qs.at(-1)?.get('user_id')).toBeNull()
  await expect.poll(() => qs.at(-1)?.get('model')).toBe('gpt-5')
})

test('运维退款：查无此单、未扣费禁退、already_refunded 走提示并把预览翻成已退款', async ({ page }) => {
  await prepare(page)
  await page.route('**/admin/billing/record/**', (route) => {
    const id = new URL(route.request().url()).pathname.split('/').pop()
    if (id === 'missing-id') return route.fulfill(apiError(404, 'record_not_found'))
    if (id === 'not-billed') {
      return route.fulfill({
        json: {
          request_id: id, user_id: 7, username: 'alice', model: 'gpt-5', status: 10,
          amount_micro: 0, prompt_tokens: 10, completion_tokens: 0, error_code: 'upstream_error',
          created_at: '2026-09-08T00:00:00Z', refundable: false,
        },
      })
    }
    return route.fulfill({
      json: {
        request_id: id, user_id: 7, username: 'alice', model: 'gpt-5', status: 20,
        amount_micro: 1_230_000, prompt_tokens: 10, completion_tokens: 20, error_code: null,
        created_at: '2026-09-08T00:00:00Z', refundable: true,
      },
    })
  })
  await page.route('**/admin/billing/refund', (route) =>
    route.fulfill({ json: { outcome: 'already_refunded' } }),
  )

  await page.goto('/admin/ops')
  await page.locator('#rid').fill('missing-id')
  await page.getByRole('button', { name: '查询这笔账' }).click()
  await expect(page.getByRole('alert')).toContainText('记录不存在')

  await page.locator('#rid').fill('not-billed')
  await page.getByRole('button', { name: '查询这笔账' }).click()
  await expect(page.getByText('未成功扣费')).toBeVisible()
  await expect(page.getByText('这笔请求没有成功扣费')).toBeVisible()
  await expect(page.getByRole('button', { name: '退款', exact: true })).toBeDisabled()

  await page.locator('#rid').fill('already-id')
  await page.getByRole('button', { name: '查询这笔账' }).click()
  await expect(page.getByText('已扣费，可退款')).toBeVisible()
  await page.getByRole('button', { name: '退款', exact: true }).click()
  await page.getByRole('alertdialog').getByRole('button', { name: '退款', exact: true }).click()
  await expect(page.getByRole('status').filter({ hasText: '此前已退过款' })).toBeVisible()
  await expect(page.getByText('已退款', { exact: true })).toBeVisible()
  await expect(page.getByRole('button', { name: '退款', exact: true })).toBeDisabled()
})

test('经营报表：资金流入四桶 micro→USD；分组表 days 进查询；切窗重拉', async ({ page }) => {
  await prepare(page)
  const daysQ: { path: string; days: string }[] = []
  await page.route('**/admin/stats/cashflow?*', (route) => {
    const days = new URL(route.request().url()).searchParams.get('days') ?? ''
    daysQ.push({ path: 'cashflow', days })
    return route.fulfill({
      json: {
        today: { recharge_micro: 0, granted_micro: 0, clawed_micro: 0, expired_micro: 0 },
        window: { recharge_micro: 1_230_000, granted_micro: 2_000_000, clawed_micro: 100_000, expired_micro: 50_000 },
      },
    })
  })
  await page.route('**/admin/stats/groups?*', (route) => {
    const days = new URL(route.request().url()).searchParams.get('days') ?? ''
    daysQ.push({ path: 'groups', days })
    return route.fulfill({
      json: {
        data: [{
          group: 'vip', group_ratio: '0.800000', requests: 12, tokens: 1500,
          amount_micro: 1_230_000, share_bp: 6000, discount_micro: 200_000,
          errors: 1, error_rate_bp: 250,
        }],
      },
    })
  })
  await page.route('**/admin/stats/margin*', (route) =>
    route.fulfill({
      json: {
        days: 7,
        window: { start_date: '2026-09-02', end_date: '2026-09-08', timezone: 'UTC' },
        data: [{ day: '2026-09-08', requests: 12, amount_micro: 1_230_000, discount_micro: 200_000 }],
        total: {
          requests: 12, errors: 1, error_rate_bp: 250,
          amount_micro: 1_230_000, discount_micro: 200_000,
          upstream_cost_micro: 800_000, margin_micro: 400_000, margin_rate_bp: 3252,
          cost_known_requests: 12, known_cost_micro: 800_000,
          known_margin_micro: 400_000, cost_coverage_bp: 8000,
        },
      },
    }),
  )

  await page.goto('/admin/revenue')
  await expect.poll(() => daysQ.some((q) => q.path === 'cashflow' && q.days === '7')).toBe(true)
  await expect.poll(() => daysQ.some((q) => q.path === 'groups' && q.days === '7')).toBe(true)
  await expect(page.getByText('资金流入（窗口）')).toBeVisible()
  await expect(page.getByText(/充值.*\$1\.23/)).toBeVisible()
  await expect(page.getByText(/兑换与补偿入账.*\$2\.00/)).toBeVisible()
  await expect(page.getByText(/管理扣减.*\$0\.10/)).toBeVisible()
  await expect(page.getByText(/过期清零.*\$0\.05/)).toBeVisible()
  await expect(page.getByText('vip')).toBeVisible()
  await expect(page.getByText('×0.800000').or(page.getByText('×0.8'))).toBeVisible()
  await expect(page.getByText(/\$1\.23/).first()).toBeVisible()
  await expect(page.getByText(/已采集部分毛利.*\$0\.40/)).toBeVisible()
  await expect(page.getByText(/成本覆盖率 80/)).toBeVisible()

  await page.getByRole('button', { name: '近 30 天' }).click()
  await expect.poll(() => daysQ.some((q) => q.path === 'cashflow' && q.days === '30')).toBe(true)
  await expect.poll(() => daysQ.some((q) => q.path === 'groups' && q.days === '30')).toBe(true)

  await page.route('**/admin/stats/cashflow?*', (route) => {
    if (route.request().isNavigationRequest()) return route.fallback()
    return route.fulfill(apiError(500, 'internal_error'))
  })
  await page.getByRole('button', { name: '近 1 天' }).click()
  await expect(page.getByText('资金流入（窗口）')).toHaveCount(0)

  await page.route('**/admin/stats/groups?*', (route) => {
    if (route.request().isNavigationRequest()) return route.fallback()
    return route.fulfill(apiError(500, 'internal_error'))
  })
  await page.getByRole('button', { name: '近 30 天' }).click()
  await expect(page.getByText('按分组（价格分层视角：倍率与收入占比同列）')).toHaveCount(0)

  await page.route('**/admin/stats/margin*', (route) => {
    if (route.request().isNavigationRequest()) return route.fallback()
    return route.fulfill(apiError(500, 'internal_error'))
  })
  await page.getByRole('button', { name: '近 7 天' }).click()
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误，请稍后再试' })).toBeVisible()
  await expect(page.getByRole('button', { name: '重试' })).toBeVisible()

  await page.route('**/admin/stats/margin*', (route) => {
    if (route.request().isNavigationRequest()) return route.fallback()
    const days = Number(new URL(route.request().url()).searchParams.get('days') ?? 7)
    return route.fulfill({
      json: {
        days,
        window: { start_date: '2026-09-08', end_date: '2026-09-08', timezone: 'UTC' },
        data: [],
        total: {
          requests: 0, errors: 0, error_rate_bp: 0,
          amount_micro: 0, discount_micro: 0, upstream_cost_micro: 0, margin_micro: 0, margin_rate_bp: null,
          cost_known_requests: 0, known_cost_micro: 0, known_margin_micro: 0, cost_coverage_bp: 0,
        },
      },
    })
  })
  await page.getByRole('button', { name: '重试' }).click()
  await expect(page.getByText('窗口内还没有调用记录。')).toBeVisible()
})

test('模型删除：确认框手输名称后 DELETE；需发布时提示 epoch', async ({ page }) => {
  await prepare(page)
  const calls: { path: string; method: string }[] = []
  await page.route('**/admin/models?*', (route) => {
    if (route.request().method() !== 'GET') return route.fallback()
    return route.fulfill({
      json: {
        data: [{
          model_name: 'gpt-5', vendor: 'openai', status: 1, pricing_mode: 'ratio',
          model_ratio: '1', completion_ratio: '1', cache_ratio: null, cache_write_ratio: null,
          tier_expr: null, audio_ratio: null, audio_completion_ratio: null, image_ratio: null,
          per_call_price_micro: null, fallback_models: [],
        }],
        total: 1,
        unpriced: 0,
      },
    })
  })
  await page.route('**/admin/models/gpt-5', async (route) => {
    calls.push({ path: new URL(route.request().url()).pathname, method: route.request().method() })
    await route.fulfill({ json: { requires_publish: true } })
  })

  await page.goto('/admin/pricing')
  await page.getByRole('button', { name: '删除', exact: true }).click()
  const confirm = page.getByRole('alertdialog')
  await expect(confirm).toContainText('gpt-5')
  await expect(confirm.getByRole('button', { name: '删除', exact: true })).toBeDisabled()
  await confirm.locator('#confirm-text').fill('gpt-5')
  await page.route('**/admin/models/gpt-5', (route) => {
    if (route.request().method() !== 'DELETE') return route.fallback()
    return route.fulfill(apiError(500, 'internal_error'))
  })
  await confirm.getByRole('button', { name: '删除', exact: true }).click()
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误' })).toBeVisible()
  await page.getByRole('alert').getByRole('button', { name: '关闭', exact: true }).click()
  await page.route('**/admin/models/gpt-5', async (route) => {
    calls.push({ path: new URL(route.request().url()).pathname, method: route.request().method() })
    await route.fulfill({ json: { requires_publish: true } })
  })
  await page.getByRole('button', { name: '删除', exact: true }).click()
  const retry = page.getByRole('alertdialog')
  await retry.locator('#confirm-text').fill('gpt-5')
  await retry.getByRole('button', { name: '删除', exact: true }).click()
  await expect.poll(() => calls).toEqual([{ path: '/admin/models/gpt-5', method: 'DELETE' }])
  await expect(page.getByRole('status').filter({ hasText: '需发布定价 epoch' })).toBeVisible()
})

test('用量分析拆分：by/limit 进查询；日志链 hours=天数×24；聚焦把行变成过滤', async ({ page }) => {
  await prepare(page)
  const qs: URLSearchParams[] = []
  const metrics = {
    requests: 12, errors: 0, error_rate_bp: 0, prompt_tokens: 100, cached_tokens: 0,
    completion_tokens: 20, reasoning_tokens: 0, tokens: 120, cache_hit_bp: 0,
    amount_micro: 1_230_000, discount_micro: 0, upstream_cost_micro: 0,
    avg_latency_ms: 800, avg_ttft_ms: 120, share_bp: 6000, request_share_bp: 5000,
    rank: 1, previous_rank: 2, previous_amount_micro: 1_000_000, delta_bp: 2300,
    cost_known_requests: 12, known_margin_micro: 500_000, cost_coverage_bp: 8000,
  }
  await page.route('**/admin/stats/trend?*', (route) =>
    route.fulfill({
      json: { days: 7, granularity: 'day', scope: {}, total: { requests: 12, amount_micro: 1_230_000 }, previous: {}, data: [] },
    }),
  )
  await page.route('**/admin/stats/breakdown?*', (route) => {
    const p = new URL(route.request().url()).searchParams
    qs.push(p)
    const by = p.get('by') ?? 'model'
    const data = by === 'channel'
      ? [{ ...metrics, key: '42', label: 'openai-main', channel_id: 42, provider: 'openai' }]
      : by === 'user'
        ? [{ ...metrics, key: '7', label: 'alice', user_id: 7 }]
        : [
            { ...metrics, key: 'gpt-5', label: 'gpt-5' },
            { ...metrics, key: 'new-m', label: 'new-m', rank: 2, previous_rank: null, delta_bp: null },
            { ...metrics, key: 'drop-m', label: 'drop-m', rank: 3, previous_rank: 1, delta_bp: -2300 },
            {
              ...metrics, key: 'flat-m', label: 'flat-m', rank: 4, previous_rank: 4, delta_bp: 0,
              known_margin_micro: -100_000, cost_coverage_bp: 4000,
            },
          ]
    return route.fulfill({
      json: { days: 7, by, scope: {}, total_amount_micro: 1_230_000, total_requests: 12, data },
    })
  })

  await page.goto('/admin/stats?view=breakdown')
  await expect.poll(() => qs.some((p) => p.get('by') === 'model' && p.get('limit') === '50' && p.get('days') === '7')).toBe(true)
  await expect(page.getByText(/\$1\.23/).first()).toBeVisible()
  await expect(page.getByText('▲1')).toBeVisible()
  await expect(page.getByText('+23%')).toBeVisible()
  await expect(page.getByText('已采集部分毛利')).toBeVisible()
  await expect(page.getByText(/\$0\.50/).first()).toBeVisible()
  await expect(page.getByText('成本覆盖率 80.0%').or(page.getByText('成本覆盖率 80%')).first()).toBeVisible()
  await expect(page.getByText('新').first()).toBeVisible()
  await expect(page.getByText('▼2')).toBeVisible()
  await expect(page.getByText('-23%')).toBeVisible()
  const flat = page.getByRole('row').filter({ hasText: 'flat-m' })
  await expect(flat.getByText('—')).toBeVisible()
  await expect(flat.getByText(/-US\$0\.10/).or(flat.getByText(/-\$0\.10/))).toBeVisible()
  await expect(flat.locator('.text-destructive').filter({ hasText: /0\.10/ })).toBeVisible()

  await page.getByRole('button', { name: '查看这一行的调用明细' }).first().click()
  await expect(page).toHaveURL(/\/admin\/logs/)
  await expect(page).toHaveURL(/model=gpt-5/)
  await expect(page).toHaveURL(/hours=168/)

  await page.goto('/admin/stats?view=breakdown')
  await page.getByRole('row').filter({ hasText: 'gpt-5' }).getByRole('button', { name: /聚焦/ }).click()
  await expect(page).toHaveURL(/model=gpt-5/)
  await expect.poll(() => qs.some((p) => p.get('model') === 'gpt-5' && p.get('by') === 'channel')).toBe(true)
  await expect(page.getByText('openai-main')).toBeVisible()

  await page.getByLabel('按').selectOption('user')
  await expect.poll(() => qs.some((p) => p.get('by') === 'user' && p.get('model') === 'gpt-5')).toBe(true)
  await expect(page.getByText('alice')).toBeVisible()

  await page.goto('/admin/stats')
  await expect(page.getByText('窗口内还没有调用记录。')).toBeVisible()

  await page.goto('/admin/stats?view=breakdown')
  await page.route('**/admin/stats/breakdown?*', (route) => {
    if (route.request().isNavigationRequest()) return route.fallback()
    return route.fulfill(apiError(500, 'internal_error'))
  })
  await page.getByLabel('按').selectOption('group')
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误，请稍后再试' })).toBeVisible()
  await expect(page.getByRole('button', { name: '重试' })).toHaveCount(0)

  await page.route('**/admin/stats/breakdown?*', (route) => {
    if (route.request().isNavigationRequest()) return route.fallback()
    return route.fulfill({
      json: { days: 7, by: 'provider', scope: {}, total_amount_micro: 0, total_requests: 0, data: [] },
    })
  })
  await page.getByLabel('按').selectOption('provider')
  await expect(page.getByText('窗口内还没有调用记录。')).toBeVisible()
})

test('渠道 key 状态与近 24h：部分可用/冷却/未入池；健康查询 days=1', async ({ page }) => {
  await prepare(page)
  const healthQ: string[] = []
  const soon = new Date(Date.now() + 10 * 60_000).toISOString()
  await page.route('**/admin/channels?*', (route) =>
    route.fulfill({
      json: {
        data: [{
          id: 42, name: 'openai-main', provider: 'openai', api_base: 'https://api.openai.com/v1',
          status: 1, priority: 0, models: ['gpt-5'],
          keys: [
            { id: 1, status: 1, failed_count: 0, cooldown_until: null, last_error: null, weight: 1, max_concurrency: null, credential_kind: 0 },
            { id: 2, status: 2, failed_count: 3, cooldown_until: soon, last_error: '429', weight: 1, max_concurrency: null, credential_kind: 1, credential_expires_at: Math.floor(Date.now() / 1000) - 120 },
          ],
          settings: {}, pools: [], pool_members: [], cost_milli: 1000,
          data_retention: null,
          last_test: { ok: true, latency_ms: 120, at: new Date().toISOString() },
          last_balance: null,
        }, {
          id: 43, name: 'idle-west', provider: 'openai', api_base: 'https://api.openai.com/v1',
          status: 1, priority: 0, models: ['gpt-5'],
          keys: [
            { id: 3, status: 1, failed_count: 0, cooldown_until: null, last_error: null, weight: 1, max_concurrency: null, credential_kind: 0 },
          ],
          settings: {}, pools: ['default'], pool_members: [], cost_milli: 1000,
          data_retention: null,
          last_test: { ok: false, latency_ms: 0, http_status: 429, at: new Date().toISOString() },
          last_balance: null,
        }, {
          id: 44, name: 'empty-keys', provider: 'openai', api_base: 'https://api.openai.com/v1',
          status: 1, priority: 0, models: ['gpt-5'],
          keys: [],
          settings: {}, pools: ['default'], pool_members: [], cost_milli: 1000,
          data_retention: null,           last_test: null, last_balance: null,
        }, {
          id: 45, name: 'parked', provider: 'openai', api_base: 'https://api.openai.com/v1',
          status: 2, priority: 0, models: ['gpt-5'],
          keys: [
            { id: 4, status: 1, failed_count: 0, cooldown_until: null, last_error: null, weight: 1, max_concurrency: null, credential_kind: 0 },
          ],
          settings: {}, pools: ['default'], pool_members: [], cost_milli: 1000,
          data_retention: null, last_test: null, last_balance: null,
        }],
        total: 4, enabled: 3,
      },
    }),
  )
  await page.route('**/admin/stats/channels?*', (route) => {
    if (route.request().url().includes('/timeline')) return route.fallback()
    healthQ.push(new URL(route.request().url()).search)
    return route.fulfill({
      json: {
        data: [{
          channel_id: 42, name: 'openai-main', provider: 'openai', requests: 80, errors: 20,
          error_rate_bp: 2500, ttft_p50_ms: 80, ttft_p95_ms: 200, ttft_p99_ms: 400,
          failovers: 0, sticky_rate_bp: 0, tokens_per_1k_sec: 0, amount_micro: 0,
        }],
      },
    })
  })

  await page.goto('/admin/channels')
  await expect.poll(() => healthQ.some((s) => s.includes('days=1') && s.includes('limit=100'))).toBe(true)
  await expect(page.getByText('1/2 可用')).toBeVisible()
  await expect(page.getByText('冷却 1')).toBeVisible()
  await expect(page.getByText(/分钟后恢复/)).toBeVisible()
  await expect(page.getByText('未入池')).toBeVisible()
  await expect(page.getByText('25.0%').or(page.getByText('25%'))).toBeVisible()
  await expect(page.getByText('80 次')).toBeVisible()
  await expect(page.getByText('token 已过期（下次请求时刷新）')).toBeVisible()
  await expect(page.getByText('120 ms')).toBeVisible()
  await expect(page.getByText('HTTP 429')).toBeVisible()
  await expect(page.getByText('没有 key')).toBeVisible()
  await expect(page.getByText('未测过').first()).toBeVisible()
  await expect(page.getByRole('row').filter({ hasText: 'idle-west' }).getByText('1 把 key 全部可用')).toBeVisible()
  await expect(page.getByRole('row').filter({ hasText: 'parked' }).getByText('停用')).toBeVisible()
  await expect(page.getByRole('row').filter({ hasText: 'idle-west' }).getByText('—')).toBeVisible()
})

test('服务质量趋势：缺省 metric=error_rate；切 stack 与天数进查询', async ({ page }) => {
  await prepare(page)
  const qs: URLSearchParams[] = []
  await page.route('**/admin/stats/trend?*', (route) => {
    qs.push(new URL(route.request().url()).searchParams)
    return route.fulfill({
      json: { days: 7, granularity: 'day', scope: {}, total: {}, previous: {}, data: [] },
    })
  })

  await page.goto('/admin/quality')
  await expect(page.locator('#main-content').getByRole('heading', { name: '服务质量' })).toBeVisible()
  await expect.poll(() => qs.some((p) => p.get('metric') === 'error_rate' && p.get('days') === '7')).toBe(true)
  await expect(page.getByText('窗口内还没有调用记录。')).toBeVisible()

  await page.getByRole('button', { name: '平均时延' }).click()
  await expect.poll(() => qs.some((p) => p.get('metric') === 'latency')).toBe(true)

  await page.locator('label').filter({ hasText: '对比维度' }).locator('select').selectOption('model')
  await expect.poll(() => qs.some((p) => p.get('stack') === 'model' && p.get('metric') === 'latency')).toBe(true)

  await page.getByRole('button', { name: '首 Token 时延' }).click()
  await expect.poll(() => qs.some((p) => p.get('metric') === 'ttft' && p.get('stack') === 'model')).toBe(true)
  await page.getByRole('button', { name: '输出吞吐量' }).click()
  await expect.poll(() => qs.some((p) => p.get('metric') === 'throughput')).toBe(true)

  // 切时间窗保留已选指标与比较维度。
  await page.getByRole('button', { name: '近 30 天' }).click()
  await expect.poll(() => qs.some((p) => p.get('days') === '30' && p.get('metric') === 'throughput' && p.get('stack') === 'model')).toBe(true)

  await page.getByText('高级筛选与比较').click()
  await page.getByLabel('时间粒度').selectOption('hour')
  await page.getByRole('button', { name: '应用分析条件' }).click()
  await expect.poll(() => qs.some((p) => p.get('granularity') === 'hour' && p.get('days') === '30')).toBe(true)
  await page.getByRole('button', { name: '重置高级条件' }).click()
  await expect.poll(() => qs.at(-1)?.get('granularity')).toBeNull()
  await page.getByLabel('开始日期').fill('2026-09-01')
  await page.getByRole('button', { name: '应用分析条件' }).click()
  await expect(page.getByRole('alert')).toContainText('请选择截至今天、包含首尾的 1–366 天有效日期')
  await page.getByLabel('开始日期').fill('2026-08-01')
  await page.getByLabel('结束日期').fill('2026-09-08')
  await page.getByLabel('时间粒度').selectOption('hour')
  const beforeHourRange = qs.length
  await page.getByRole('button', { name: '应用分析条件' }).click()
  await expect(page.getByRole('alert')).toContainText('请选择截至今天、包含首尾的 1–366 天有效日期')
  expect(qs).toHaveLength(beforeHourRange)

  await page.getByRole('button', { name: '重置高级条件' }).click()
  await page.route('**/admin/stats/trend?*', (route) => route.fulfill(apiError(500, 'internal_error')))
  await page.getByRole('button', { name: '平均时延' }).click()
  await expect(page.getByRole('alert')).toContainText('服务内部错误')
  await expect(page.getByRole('button', { name: '重试' })).toBeVisible()
})

test('团队详情用量：钱包与成员消费 micro→USD；空上限显示不限', async ({ page }) => {
  await prepare(page, { permissions: [] })
  await page.route('**/api/teams?*', (route) =>
    route.fulfill({
      json: {
        data: [{
          team_id: 9, name: 'Data Team', role: 'owner', member_count: 1,
          monthly_spend_limit_micro: null, balance_micro: 12_500_000,
        }],
        total: 1,
      },
    }),
  )
  await page.route('**/api/teams/9/usage', (route) =>
    route.fulfill({
      json: {
        team_id: 9, balance_micro: 12_500_000,
        members: [{
          member_user_id: 1, username: 'alice', role: 'owner',
          monthly_spend_limit_micro: null, total_spend_micro: 3_000_000, month_spend_micro: 250_000,
        }],
      },
    }),
  )

  await page.goto('/portal/teams')
  const listed = page.getByRole('row').filter({ hasText: 'Data Team' })
  await expect(listed.getByText('所有者')).toBeVisible()
  await expect(listed.getByText(/\$12\.50/)).toBeVisible()
  await listed.getByRole('button', { name: '管理', exact: true }).click()
  const detail = await openedDialog(page)
  await expect(detail.getByRole('heading', { name: 'Data Team' })).toBeVisible()
  await expect(detail.getByText(/\$12\.50/).first()).toBeVisible()
  const alice = detail.getByRole('row').filter({ hasText: 'alice' })
  await expect(alice).toContainText(/\$0\.25/)
  await expect(alice).toContainText(/\$3\.00/)
  await expect(alice).toContainText('不限')

  await page.route('**/api/teams/9/usage', (route) => route.fulfill(apiError(500, 'internal_error')))
  await page.reload()
  await page.getByRole('row').filter({ hasText: 'Data Team' }).getByRole('button', { name: '管理', exact: true }).click()
  const usageFail = await openedDialog(page)
  await expect(usageFail.getByRole('alert')).toContainText('服务内部错误')
  await expect(usageFail.getByText(/\$0/).first()).toBeVisible()
})

test('用量 KPI：万元紧凑记法与已采集毛利', async ({ page }) => {
  await prepare(page)
  await page.route('**/admin/stats/trend?*', (route) =>
    route.fulfill({
      json: {
        days: 7, granularity: 'day', scope: {},
        total: {
          requests: 10, amount_micro: 71_535_310_000, known_margin_micro: 500_000,
          cost_coverage_bp: 8000, tokens: 100, errors: 0, error_rate_bp: 0,
        },
        previous: {},
        data: [],
      },
    }),
  )
  await page.goto('/admin/stats')
  await expect(page.locator('#main-content').getByRole('heading', { name: '用量分析' })).toBeVisible()
  await expect(page.getByText(/7\.2万/)).toBeVisible()
  await expect(page.getByText(/已采集部分毛利/)).toBeVisible()
  await expect(page.getByText(/成本覆盖率 80/)).toBeVisible()
})

test('用户列表：搜索 q 进 URL 与查询；空结果；停用态', async ({ page }) => {
  await prepare(page)
  const qs: URLSearchParams[] = []
  await page.route('**/admin/users?*', (route) => {
    if (route.request().method() !== 'GET') return route.fallback()
    const p = new URL(route.request().url()).searchParams
    qs.push(p)
    const q = p.get('q') ?? ''
    const all = [
      { id: 7, username: 'alice', email: 'alice@ok.test', role: 1, status: 1, balance_micro: 5_000_000, admin_role_id: null, price_multiplier: '1.250000' },
      { id: 8, username: 'bob', email: 'bob@ok.test', role: 1, status: 2, balance_micro: 0, admin_role_id: null, price_multiplier: '1' },
    ]
    const data = q === '' ? all : all.filter((u) => u.username.includes(q) || (u.email ?? '').includes(q))
    return route.fulfill({ json: { total: data.length, data } })
  })

  await page.goto('/admin/users')
  await expect(page.locator('#main-content').getByRole('heading', { name: '用户列表' })).toBeVisible()
  await expect(page.getByText('共 2 人')).toBeVisible()
  await expect(page.getByRole('row').filter({ hasText: 'alice' }).getByText('启用')).toBeVisible()
  await expect(page.getByRole('row').filter({ hasText: 'bob' }).getByText('停用')).toBeVisible()
  await expect(page.getByRole('row').filter({ hasText: 'alice' }).getByText('×1.250000')).toBeVisible()

  await page.getByLabel('搜索用户名 / 邮箱（回车）').fill('  alice ')
  await page.getByRole('button', { name: '搜索', exact: true }).click()
  await expect(page).toHaveURL(/q=alice/)
  await expect.poll(() => qs.some((p) => p.get('q') === 'alice')).toBe(true)
  await expect(page.getByText('搜索「alice」的结果')).toBeVisible()
  await expect(page.getByRole('row').filter({ hasText: 'bob' })).toHaveCount(0)

  await page.getByLabel('搜索用户名 / 邮箱（回车）').fill('nobody')
  await page.getByRole('button', { name: '搜索', exact: true }).click()
  await expect(page.getByText('没有匹配的结果')).toBeVisible()

  await page.getByRole('button', { name: '清空筛选' }).click()
  await expect(page).not.toHaveURL(/q=/)
  await expect(page.getByText('共 2 人')).toBeVisible()

  await page.route('**/admin/users?*', (route) => {
    if (route.request().method() !== 'GET') return route.fallback()
    return route.fulfill({ json: { total: 0, data: [] } })
  })
  await page.reload()
  await expect(page.getByText('暂无数据')).toBeVisible()

  await page.route('**/admin/users?*', (route) => {
    if (route.request().method() !== 'GET') return route.fallback()
    return route.fulfill(apiError(500, 'internal_error'))
  })
  await page.reload()
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误，请稍后再试' })).toBeVisible()
  await expect(page.getByRole('button', { name: '重试' })).toBeVisible()
})

test('门户日志 CSV：六位 USD、公式注入前缀、失败行 status；全账户才带 key 列', async ({ page }) => {
  await prepare(page)
  await page.route('**/api/me/logs?*', (route) => {
    const scope = new URL(route.request().url()).searchParams.get('scope') ?? 'key'
    return route.fulfill({
      json: {
        scope,
        data: [
          portalLog(1, { model: '=1+2' }),
          portalLog(2, { status: 40, error_code: 'upstream_error' }),
        ],
        next_before: null,
      },
    })
  })

  await page.goto('/portal/logs')
  await expect(page.locator('#main-content').getByRole('heading', { name: '用量日志' })).toBeVisible()

  const [keyCsv] = await Promise.all([
    page.waitForEvent('download'),
    page.getByRole('button', { name: '导出已加载 CSV' }).click(),
  ])
  expect(keyCsv.suggestedFilename()).toMatch(/^okapi-usage-/)
  const withoutKey = await csvFrom(keyCsv)
  expect(withoutKey.split('\n')[0]).toBe(
    'time,billing_status,model,requested_model,endpoint,stream,prompt_tokens,cached_tokens,cache_read_reported,cache_write_tokens,cache_write_reported,completion_tokens,reasoning_tokens,net_amount_usd,charged_amount_usd,original_usd,discount_usd,refunded_amount_usd,latency_ms,ttft_ms,error_code,request_id',
  )
  expect(withoutKey).toContain(',settled,\'=1+2,,,true,100,20,,,,40,8,0.240000,0.240000,0.300000,0.060000,0.000000,800,120,,req-1')
  expect(withoutKey).toContain(',failed,gpt-5,')

  await page.getByRole('button', { name: '全账户' }).click()
  const [userCsv] = await Promise.all([
    page.waitForEvent('download'),
    page.getByRole('button', { name: '导出已加载 CSV' }).click(),
  ])
  const withKey = await csvFrom(userCsv)
  expect(withKey.split('\n')[0]).toMatch(/^time,billing_status,key,key_id,model,/)
  expect(withKey).toContain(',web,1,\'=1+2,')
})

test('渠道列表搜索：q trim 与协议进 URL；空结果可清空', async ({ page }) => {
  await prepare(page)
  const qs: URLSearchParams[] = []
  const ch = (over: Json) => ({
    id: 42, name: 'openai-main', provider: 'openai', api_base: 'https://api.openai.com/v1',
    status: 1, priority: 0, models: ['gpt-5', 'o1'],
    keys: [{ id: 1, status: 1, failed_count: 0, cooldown_until: null, last_error: null, weight: 1, max_concurrency: null, credential_kind: 0 }],
    settings: {}, pools: ['default'], pool_members: [], cost_milli: 1000,
    data_retention: null, last_test: null,
    last_balance: {
      probe: 'openai', currency: 'CNY', balance_micro: 110_500_000,
      total_micro: null, used_micro: null, at: new Date().toISOString(),
    },
    ...over,
  })
  await page.route('**/admin/channels?*', (route) => {
    const p = new URL(route.request().url()).searchParams
    qs.push(p)
    const q = p.get('q') ?? ''
    const provider = p.get('provider') ?? ''
    let data = [
      ch({}),
      ch({ id: 43, name: 'claude-west', provider: 'anthropic', last_balance: null, models: ['claude-sonnet'] }),
      ch({
        id: 44, name: 'proxy-east', provider: 'openai_compat',
        api_base: 'https://proxy.example.com/v1', last_balance: null, models: ['gpt-5'],
      }),
      ch({
        id: 45, name: 'pass-west', provider: 'custom_pass', api_base: 'not-a-url',
        last_balance: null, models: ['passthru'],
      }),
      ch({
        id: 46, name: 'broke-east', provider: 'openai_compat', api_base: null, last_balance: {
          probe: 'openai', currency: 'USD', balance_micro: 0,
          total_micro: null, used_micro: null, at: new Date().toISOString(),
        }, models: ['gpt-5'],
      }),
    ]
    if (q) data = data.filter((c) => String(c.name).includes(q))
    if (provider) data = data.filter((c) => c.provider === provider)
    return route.fulfill({ json: { data, total: data.length, enabled: data.length } })
  })

  await page.goto('/admin/channels')
  await expect(page.locator('#main-content').getByRole('heading', { name: '渠道' })).toBeVisible()
  await expect(page.getByRole('row').filter({ hasText: 'claude-west' })).toBeVisible()
  const openai = page.getByRole('row').filter({ hasText: 'openai-main' })
  await expect(openai.getByRole('link', { name: '打开供应商控制台' })).toHaveAttribute(
    'href',
    'https://platform.openai.com/usage',
  )
  await expect(page.getByRole('row').filter({ hasText: 'claude-west' }).getByRole('link', { name: '打开供应商控制台' }))
    .toHaveAttribute('href', 'https://console.anthropic.com/settings/usage')
  await expect(page.getByRole('row').filter({ hasText: 'proxy-east' }).getByRole('link', { name: '打开供应商控制台' }))
    .toHaveAttribute('href', 'https://proxy.example.com')
  await expect(page.getByRole('row').filter({ hasText: 'pass-west' }).getByRole('link', { name: '打开供应商控制台' }))
    .toHaveCount(0)
  await expect(page.getByRole('row').filter({ hasText: 'broke-east' }).getByRole('link', { name: '打开供应商控制台' }))
    .toHaveCount(0)
  await expect(openai.getByText('2 个')).toBeVisible()
  await expect(openai.getByText(/¥110\.50/)).toBeVisible()
  await expect(page.getByRole('row').filter({ hasText: 'broke-east' }).getByText(/\$0\.00/)).toBeVisible()

  await page.getByPlaceholder('按名称或地址搜索').fill('  openai ')
  await page.getByRole('button', { name: '搜索', exact: true }).click()
  await expect(page).toHaveURL(/q=openai/)
  await expect.poll(() => qs.some((p) => p.get('q') === 'openai')).toBe(true)
  await expect(page.getByRole('row').filter({ hasText: 'claude-west' })).toHaveCount(0)

  await page.getByLabel('协议').selectOption('anthropic')
  await expect(page).toHaveURL(/provider=anthropic/)
  await expect.poll(() => qs.some((p) => p.get('provider') === 'anthropic' && p.get('q') === 'openai')).toBe(true)
  await expect(page.getByText('没有匹配的结果')).toBeVisible()

  await page.getByRole('button', { name: '清空筛选' }).first().click()
  await expect(page).not.toHaveURL(/q=/)
  await expect(page).not.toHaveURL(/provider=/)
  await expect(page.getByRole('row').filter({ hasText: 'claude-west' })).toBeVisible()

  await page.route('**/admin/channels?*', (route) => {
    if (route.request().isNavigationRequest()) return route.fallback()
    return route.fulfill({ json: { data: [], total: 0, enabled: 0 } })
  })
  await page.reload()
  await expect(page.getByText('尚无渠道。接入第一家上游后，模型才有地方可打。')).toBeVisible()
  await page
    .getByText('尚无渠道。接入第一家上游后，模型才有地方可打。')
    .locator('xpath=ancestor::div[contains(@class,"border-dashed")]')
    .getByRole('button', { name: '新建渠道' })
    .click()
  await expect(page.getByRole('dialog').getByRole('heading', { name: '新建渠道' })).toBeVisible()

  await page.route('**/admin/channels?*', (route) => {
    if (route.request().isNavigationRequest() || route.request().method() !== 'GET') return route.fallback()
    return route.fulfill(apiError(500, 'internal_error'))
  })
  await page.reload()
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误，请稍后再试' })).toBeVisible()
  await expect(page.getByRole('button', { name: '重试' })).toBeVisible()
})

test('兑换码停用整批：未使用可删、已核销禁点；DELETE 批次号且 toast 张数', async ({ page }) => {
  await prepare(page)
  const unusedBatch = '11111111-1111-1111-1111-111111111111'
  const usedBatch = '22222222-2222-2222-2222-222222222222'
  const deletes: string[] = []
  await page.route('**/admin/redemptions?*', (route) =>
    route.fulfill({
      json: {
        total: 2,
        data: [
          {
            id: 1, batch_id: unusedBatch, amount_micro: 1_230_000, status: 1,
            plan_code: 'starter', bind_user_id: null, redeemed_by: null, redeemed_at: null,
            created_at: '2026-09-05T09:00:00Z',
          },
          {
            id: 2, batch_id: usedBatch, amount_micro: 500_000, status: 2,
            plan_code: null, bind_user_id: null, redeemed_by: 7, redeemed_at: '2026-09-06T10:00:00Z',
            created_at: '2026-09-05T09:00:00Z',
          },
        ],
      },
    }),
  )
  await page.route('**/admin/redemptions/*', async (route) => {
    if (route.request().method() !== 'DELETE') return route.fallback()
    deletes.push(new URL(route.request().url()).pathname)
    await route.fulfill({ json: { affected: 3 } })
  })

  await page.goto('/admin/codes')
  await expect(page.locator('#main-content').getByRole('heading', { name: '兑换码' })).toBeVisible()
  const unused = page.getByRole('row').filter({ hasText: '11111111' })
  const used = page.getByRole('row').filter({ hasText: '22222222' })
  await expect(unused.getByText(/\$1\.23/)).toBeVisible()
  await expect(unused.getByText('未使用')).toBeVisible()
  await expect(unused.getByText('starter')).toBeVisible()
  await expect(used.getByText('已使用')).toBeVisible()
  await expect(used.getByText('7', { exact: true })).toBeVisible()
  await expect(used.getByRole('button', { name: '停用整批' })).toBeDisabled()

  await unused.getByRole('button', { name: '停用整批' }).click()
  const confirm = page.getByRole('alertdialog')
  await expect(confirm.getByRole('heading', { name: '停用整批' })).toBeVisible()
  await expect(confirm).toContainText('整批未核销的兑换码将被停用')
  await page.route('**/admin/redemptions/*', (route) => {
    if (route.request().method() !== 'DELETE') return route.fallback()
    return route.fulfill(apiError(500, 'internal_error'))
  })
  await confirm.getByRole('button', { name: '停用整批' }).click()
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误' })).toBeVisible()
  await page.getByRole('alert').getByRole('button', { name: '关闭', exact: true }).click()
  await page.route('**/admin/redemptions/*', async (route) => {
    if (route.request().method() !== 'DELETE') return route.fallback()
    deletes.push(new URL(route.request().url()).pathname)
    await route.fulfill({ json: { affected: 3 } })
  })
  await unused.getByRole('button', { name: '停用整批' }).click()
  await page.getByRole('alertdialog').getByRole('button', { name: '停用整批' }).click()
  await expect.poll(() => deletes).toEqual([`/admin/redemptions/${unusedBatch}`])
  await expect(page.getByRole('status').filter({ hasText: '已停用 3 张未核销码' })).toBeVisible()

  await page.route('**/admin/redemptions?*', (route) => route.fulfill({ json: { total: 0, data: [] } }))
  await page.reload()
  await expect(page.getByText('还没有兑换码，点右上角生成一批。')).toBeVisible()
  await page
    .getByText('还没有兑换码，点右上角生成一批。')
    .locator('xpath=ancestor::div[contains(@class,"border-dashed")]')
    .getByRole('button', { name: '生成' })
    .click()
  await expect(page.getByRole('dialog').getByRole('heading', { name: '批量生成兑换码' })).toBeVisible()

  await page.route('**/admin/redemptions?*', (route) => {
    if (route.request().isNavigationRequest() || route.request().method() !== 'GET') return route.fallback()
    return route.fulfill(apiError(500, 'internal_error'))
  })
  await page.reload()
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误，请稍后再试' })).toBeVisible()
  await expect(page.getByRole('button', { name: '重试' })).toBeVisible()
})

test('渠道池列表：内置/空池/策略与被引用明细', async ({ page }) => {
  await prepare(page)
  await page.route('**/admin/pools?*', (route) =>
    route.fulfill({
      json: {
        data: [
          {
            pool_code: 'default', description: null, routing_strategy: 'priority_weighted',
            fallback_pool_code: null, builtin: true, channel_count: 3,
            group_count: 2, key_count: 1, fallback_ref_count: 1,
          },
          {
            pool_code: 'premium', description: 'paid', routing_strategy: 'least_latency',
            fallback_pool_code: 'default', builtin: false, channel_count: 1,
            group_count: 1, key_count: 0, fallback_ref_count: 0,
          },
          {
            pool_code: 'spare', description: null, routing_strategy: 'priority_weighted',
            fallback_pool_code: null, builtin: false, channel_count: 0,
            group_count: 0, key_count: 0, fallback_ref_count: 0,
          },
        ],
        total: 3,
      },
    }),
  )

  await page.goto('/admin/pools')
  await expect(page.locator('#main-content').getByRole('heading', { name: '渠道池' })).toBeVisible()
  const def = page.getByRole('row').filter({ hasText: 'default' }).first()
  await expect(def.getByText('内置')).toBeVisible()
  await expect(def.getByText('优先级 + 加权')).toBeVisible()
  await expect(def.getByText('2 个分组 / 1 个令牌')).toBeVisible()
  await expect(def.getByText('1 个池的降级目标')).toBeVisible()
  await expect(page.getByRole('row').filter({ hasText: 'premium' }).getByText('最低时延')).toBeVisible()
  await expect(page.getByRole('row').filter({ hasText: 'premium' }).getByText('default')).toBeVisible()
  await expect(page.getByRole('row').filter({ hasText: 'spare' }).getByText('空池')).toBeVisible()
  await expect(page.getByRole('row').filter({ hasText: 'spare' }).getByText('—').first()).toBeVisible()

  await page.route('**/admin/pools?*', (route) => route.fulfill({ json: { data: [], total: 0 } }))
  await page.reload()
  await expect(page.getByText('只有内置 default 池：所有分组走它，新渠道也缺省进它。需要按档位区分上游时再建新池。')).toBeVisible()
  await page
    .getByText('只有内置 default 池：所有分组走它，新渠道也缺省进它。需要按档位区分上游时再建新池。')
    .locator('xpath=ancestor::div[contains(@class,"border-dashed")]')
    .getByRole('button', { name: '新建池' })
    .click()
  await expect(page.getByRole('dialog').getByRole('heading', { name: '新建池' })).toBeVisible()

  await page.route('**/admin/pools?*', (route) => {
    if (route.request().isNavigationRequest() || route.request().method() !== 'GET') return route.fallback()
    return route.fulfill(apiError(500, 'internal_error'))
  })
  await page.reload()
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误，请稍后再试' })).toBeVisible()
  await expect(page.getByRole('button', { name: '重试' })).toBeVisible()
})

test('管理端套餐列表：充值模板与订阅列不同；售价 0 显示不售卖', async ({ page }) => {
  await prepare(page)
  await page.route('**/admin/plans?*', (route) =>
    route.fulfill({
      json: {
        total: 2,
        data: [
          {
            id: 1, plan_code: 'starter', display_name: 'Starter', kind: 0,
            grant_micro: 10_000_000, group_code: 'vip', balance_valid_days: 90,
            price_micro: 0, period: null, duration_days: null, sort_order: 5,
            description: null, status: 1, code_count: 3, active_subscribers: 0,
          },
          {
            id: 2, plan_code: 'pro', display_name: 'Pro', kind: 1,
            grant_micro: 5_000_000, group_code: null, balance_valid_days: null,
            price_micro: 0, period: 1, duration_days: 30, sort_order: 1,
            description: null, status: 1, code_count: 0, active_subscribers: 12,
          },
        ],
      },
    }),
  )

  await page.goto('/admin/plans')
  await expect(page.locator('#main-content').getByRole('heading', { name: '套餐' })).toBeVisible()
  const starter = page.getByRole('row').filter({ hasText: 'starter' })
  await expect(starter.getByText('充值模板')).toBeVisible()
  await expect(starter.getByText(/\$10\.00/)).toBeVisible()
  await expect(starter.getByText('vip')).toBeVisible()
  await expect(starter.getByText('90')).toBeVisible()
  await expect(starter.getByText('3')).toBeVisible()
  await expect(starter.getByText('—').first()).toBeVisible()
  const pro = page.getByRole('row').filter({ hasText: 'pro' })
  await expect(pro.getByText('订阅')).toBeVisible()
  await expect(pro.getByText(/\$5\.00/)).toBeVisible()
  await expect(pro.getByText('每日')).toBeVisible()
  await expect(pro.getByText('不售卖')).toBeVisible()
  await expect(pro.getByText('30')).toBeVisible()
  await expect(pro.getByText('12')).toBeVisible()

  await page.route('**/admin/plans?*', (route) => route.fulfill({ json: { total: 0, data: [] } }))
  await page.reload()
  await expect(page.getByText('还没有套餐。想让兑换码同时改分组或设有效期时才需要套餐。')).toBeVisible()
  await page
    .getByText('还没有套餐。想让兑换码同时改分组或设有效期时才需要套餐。')
    .locator('xpath=ancestor::div[contains(@class,"border-dashed")]')
    .getByRole('button', { name: '新建套餐' })
    .click()
  await expect(page.getByRole('dialog').getByRole('heading', { name: '新建套餐' })).toBeVisible()

  await page.route('**/admin/plans?*', (route) => {
    if (route.request().isNavigationRequest() || route.request().method() !== 'GET') return route.fallback()
    return route.fulfill(apiError(500, 'internal_error'))
  })
  await page.reload()
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误，请稍后再试' })).toBeVisible()
  await expect(page.getByRole('button', { name: '重试' })).toBeVisible()
})

test('角色列表：权限点截断为前 4 个并显示其余项数', async ({ page }) => {
  await prepare(page)
  await page.route('**/admin/roles?*', (route) =>
    route.fulfill({
      json: {
        total: 1,
        data: [{
          id: 3, role_code: 'ops_readonly', display_name: '只读运维',
          permissions: ['logs.read', 'audit.read', 'users.read', 'channels.read', 'ops.read'],
        }],
      },
    }),
  )

  await page.goto('/admin/roles')
  await expect(page.locator('#main-content').getByRole('heading', { name: '角色与权限' })).toBeVisible()
  await expect(page.getByText('共 1 条')).toBeVisible()
  const row = page.getByRole('row').filter({ hasText: 'ops_readonly' })
  await expect(row.getByText('只读运维')).toBeVisible()
  await expect(row.getByText('logs.read')).toBeVisible()
  await expect(row.getByText('ops.read')).toHaveCount(0)
  await expect(row.getByText('+1 项')).toBeVisible()

  await page.route('**/admin/roles?*', (route) => route.fulfill({ json: { total: 0, data: [] } }))
  await page.reload()
  await expect(page.getByText('还没有自定义角色。内置三档（用户/管理员/超管）无需在此配置。')).toBeVisible()
  await page
    .getByText('还没有自定义角色。内置三档（用户/管理员/超管）无需在此配置。')
    .locator('xpath=ancestor::div[contains(@class,"border-dashed")]')
    .getByRole('button', { name: '新建角色' })
    .click()
  await expect(page.getByRole('dialog').getByRole('heading', { name: '新建角色' })).toBeVisible()

  await page.route('**/admin/roles?*', (route) => {
    if (route.request().isNavigationRequest() || route.request().method() !== 'GET') return route.fallback()
    return route.fulfill(apiError(500, 'internal_error'))
  })
  await page.reload()
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误，请稍后再试' })).toBeVisible()
  await expect(page.getByRole('button', { name: '重试' })).toBeVisible()
})

test('计费规则列表：阶梯/时段参数人话化、叠加标签与范围', async ({ page }) => {
  await prepare(page)
  await page.route('**/admin/pricing/rules?*', (route) =>
    route.fulfill({
      json: {
        total: 3,
        data: [
          {
            rule_code: 'heavy-users', rule_type: 'volume', priority: 10, enabled: true,
            valid_from: null, valid_to: null,
            scope: { groups: ['vip'], models: ['gpt-5'], users: [7, 8] },
            params: {
              multiplier: '0.9', min_monthly_tokens: 100,
              min_monthly_spend_micro: 1_000_000, stacking_mode: 'best_for_user',
            },
          },
          {
            rule_code: 'night-discount', rule_type: 'time_based', priority: 5, enabled: false,
            valid_from: null, valid_to: null,
            scope: { groups: ['vip'] },
            params: {
              multiplier: '0.5', start_minute: 0, end_minute: 359,
              weekdays: [1, 5], stacking_mode: 'exclusive',
            },
          },
          {
            rule_code: 'flat-cut', rule_type: 'discount', priority: 0, enabled: true,
            valid_from: null, valid_to: null,
            scope: {},
            params: { multiplier: '0.8', stacking_mode: 'stackable' },
          },
        ],
      },
    }),
  )

  await page.goto('/admin/rules')
  await expect(page.locator('#main-content').getByRole('heading', { name: '计费规则' })).toBeVisible()
  const heavy = page.getByRole('row').filter({ hasText: 'heavy-users' })
  await expect(heavy.getByText('月用量 ≥ 100 tokens + 月消费 ≥ $1 时 ×0.9')).toBeVisible()
  await expect(heavy.getByText('对用户最优一条')).toBeVisible()
  await expect(heavy.getByText('分组 vip · 模型 gpt-5 · 2 个用户')).toBeVisible()
  const night = page.getByRole('row').filter({ hasText: 'night-discount' })
  await expect(night.getByText('周一五 每天 00:00–05:59 ×0.5')).toBeVisible()
  await expect(night.getByText('独占（priority 高者）')).toBeVisible()
  await expect(night.getByText('停用')).toBeVisible()
  await expect(page.getByRole('row').filter({ hasText: 'flat-cut' }).getByText('×0.8')).toBeVisible()
  await expect(page.getByRole('row').filter({ hasText: 'flat-cut' }).getByText('全部')).toBeVisible()

  await page.route('**/admin/pricing/rules?*', (route) => route.fulfill({ json: { total: 0, data: [] } }))
  await page.reload()
  await expect(page.getByText('还没有规则。折扣、时段、阶梯活动都在这里配置。')).toBeVisible()
  await page
    .getByText('还没有规则。折扣、时段、阶梯活动都在这里配置。')
    .locator('xpath=ancestor::div[contains(@class,"border-dashed")]')
    .getByRole('button', { name: '新建规则' })
    .click()
  await expect(page.getByRole('dialog').getByRole('heading', { name: '新建规则' })).toBeVisible()

  await page.route('**/admin/pricing/rules?*', (route) => {
    if (route.request().isNavigationRequest() || route.request().method() !== 'GET') return route.fallback()
    return route.fulfill(apiError(500, 'internal_error'))
  })
  await page.reload()
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误，请稍后再试' })).toBeVisible()
  await expect(page.getByRole('button', { name: '重试' })).toBeVisible()
})
