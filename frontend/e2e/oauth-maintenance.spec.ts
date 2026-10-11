import { expect, test } from '@playwright/test'
import type { Page } from '@playwright/test'
import { fileURLToPath } from 'node:url'

const providerMetadata = { data: [
  { id: 'openai', account: { quota: false, refresh: false, subscription: null } },
  { id: 'openai_compat', account: { quota: false, refresh: false, subscription: null } },
  { id: 'anthropic', account: { quota: false, refresh: false, subscription: null } },
  { id: 'azure', account: { quota: false, refresh: false, subscription: null } },
  { id: 'gemini', account: { quota: false, refresh: false, subscription: null } },
  { id: 'bedrock', account: { quota: false, refresh: false, subscription: null } },
  { id: 'vertex', account: { quota: false, refresh: false, subscription: null } },
  { id: 'custom_pass', account: { quota: false, refresh: false, subscription: null } },
  { id: 'anthropic_max', account: { authorization: { code_format: 'code_state', access_token_prefix: 'sk-ant-oat', account_id_required: false, import_profile: { name: 'claude-code', mode: 'mimic', revision: '2.1.290' } }, quota: true, refresh: true, subscription: { quota_scope: 'session', window_secs: 18000 } } },
  { id: 'codex', account: { authorization: { code_format: 'callback_url', access_token_prefix: null, account_id_required: true }, quota: true, refresh: true, subscription: { quota_scope: 'total', window_secs: null } } },
] }
async function prepare(page: Page) {
  await page.addInitScript(() => {
    localStorage.setItem('okapi.key', 'oauth-ui-fixture')
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
      } : path === '/admin/channels/providers' ? providerMetadata : path === '/admin/channels/42/usage' ? { timezone: 'UTC', usage: { window_start: '2026-10-01T00:00:00Z', window_end: '2026-10-02T00:00:00Z', requests: 0, tokens: 0, cost_micro: 0, unknown_cost_requests: 0 }, quotas: [] } : path === '/api/notice' ? { notice: null } : path.startsWith('/admin/settings/') ? { value: null } : { data: [] } })
    }
    return route.continue()
  })
}

async function openKey(page: Page, status: number) {
  await prepare(page)
  await page.route('**/admin/channels?*', (route) => route.fulfill({ json: {
    data: [{ id: 42, name: 'my-subscription', provider: 'codex', api_base: 'https://chatgpt.com/backend-api/codex',
      status: 1, priority: 0, models: ['fixture-model'], settings: {}, pools: ['default'], pool_members: [],
      cost_milli: 1000, data_retention: null, last_test: null, last_balance: null,
      keys: [{ id: 7, status, weight: 10, max_concurrency: null, cooldown_until: null,
        failed_count: 1, credential_kind: 1, credential_expires_at: 2_000_000_000,
        oauth_refresh: { last_attempt_at: 1_999_900_000, last_success_at: null, consecutive_failures: 1,
          next_retry_at: 2_000_000_000, error_code: status === 6 ? 'oauth_invalid_grant' : 'oauth_refresh_status_503' },
      }],
    }], total: 1, enabled: 1,
  } }))
  await page.goto('/admin/channels')
  await page.getByRole('row').filter({ hasText: 'my-subscription' }).getByRole('button', { name: '编辑', exact: true }).click()
  // 凭证状态（到期、刷新、重新授权）在默认的接入信息页签
  return page.getByRole('dialog')
}

for (const width of [390, 1366]) {
  for (const editing of [false, true]) {
    test(`订阅接入间距 ${width}px ${editing ? '编辑' : '新建'}：授权按钮与分隔线留白，切换及展开保持间距`, async ({ page }) => {
      await prepare(page)
      await page.setViewportSize({ width, height: 900 })
      await page.emulateMedia({ reducedMotion: 'reduce' })
      await page.addInitScript((dark) => localStorage.setItem('okapi.theme', dark ? 'dark' : 'light'), width === 390)
      if (editing) await page.route('**/admin/channels?*', (route) => route.fulfill({ json: {
        data: [{ id: 42, name: 'subscription-spacing', provider: 'anthropic_max', api_base: '', status: 1,
          priority: 0, models: ['fixture-model'], settings: {}, pools: ['default'], pool_members: [],
          cost_milli: 1000, data_retention: null, last_test: null, last_balance: null,
          keys: [{ id: 7, status: 1, weight: 10, max_concurrency: null, cooldown_until: null,
            failed_count: 0, credential_kind: 1, oauth_refreshable: true }],
        }], total: 1, enabled: 1,
      } }))
      await page.goto('/admin/channels')
      if (editing) await page.getByRole('row').filter({ hasText: 'subscription-spacing' }).getByRole('button', { name: '编辑', exact: true }).click()
      else await page.getByRole('button', { name: '新建渠道', exact: true }).first().click()
      const drawer = page.getByRole('dialog')
      if (!editing) {
        await drawer.getByLabel('渠道名', { exact: true }).fill('subscription-spacing')
        await drawer.locator('#d-provider').selectOption('anthropic_max')
      }
      const methods = drawer.getByRole('tablist', { name: '凭证接入方式', exact: true })
      const section = drawer.locator('[data-slot="channel-auth-method"] + section')
      const checkSpacing = async () => {
        const [tabs, content] = await Promise.all([methods.boundingBox(), section.boundingBox()])
        expect(content!.y - tabs!.y - tabs!.height).toBeGreaterThanOrEqual(16)
        expect(content!.y - tabs!.y - tabs!.height).toBeLessThanOrEqual(20)
        const heading = (await section.getByRole('heading').boundingBox())!
        expect(heading.y - content!.y).toBeGreaterThanOrEqual(16)
        expect(await drawer.evaluate((node) => node.scrollWidth <= node.clientWidth)).toBe(true)
      }
      await expect(section.getByRole('heading')).toHaveText('用订阅账号登录')
      await checkSpacing()
      if (editing) {
        // 凭证状态是普通块、没有下内边距：下一节（出口代理）的分隔线不能贴住刷新 / 重新授权按钮
        const reauth = (await drawer.getByRole('button', { name: '重新授权', exact: true }).boundingBox())!
        const egress = (await drawer.locator('section:has(> #channel-egress-edit)').boundingBox())!
        expect(egress.y - reauth.y - reauth.height).toBeGreaterThanOrEqual(12)
      }
      await drawer.screenshot({ path: test.info().outputPath('authorization-spacing.png'), animations: 'disabled' })
      await methods.getByRole('tab', { name: '直接导入 Token', exact: true }).click()
      await expect(section.getByRole('heading')).toHaveText('直接导入 Token')
      await checkSpacing()
      await drawer.locator('#channel-connection-options > summary').click()
      await checkSpacing()
      await drawer.locator('#channel-connection-options > summary').click()
      await methods.getByRole('tab', { name: '浏览器授权', exact: true }).click()
      await checkSpacing()
      await expect(drawer.getByLabel('渠道名', { exact: true })).toHaveValue('subscription-spacing')
    })
  }
}

test('失效 OAuth key 重新授权绑定原 key，不提供追加账号或直接启用', async ({ page }) => {
  const drawer = await openKey(page, 6)
  await expect(drawer.getByText('授权已失效，请重新登录；直接启用无法恢复凭证。')).toBeVisible()
  await expect(drawer.getByRole('button', { name: '重新启用', exact: true })).toHaveCount(0)
  await expect(drawer.getByRole('button', { name: '立即刷新凭证', exact: true })).toHaveCount(0)
  const calls: unknown[] = []
  await page.route('**/admin/channels/oauth/start', async (route) => {
    calls.push(route.request().postDataJSON())
    return route.fulfill({ json: { state: 'targeted-state', authorize_url: 'about:blank', redirect_uri: 'http://localhost/callback' } })
  })
  await page.route('**/admin/channels/oauth/exchange', async (route) => {
    calls.push(route.request().postDataJSON())
    return route.fulfill({ json: { channel_id: 42, channel_key_id: 7, expires_at: 2_000_000_000, account_id: null } })
  })
  await drawer.getByRole('button', { name: '重新授权', exact: true }).click()
  await drawer.getByRole('button', { name: '打开登录页', exact: true }).last().click()
  await drawer.getByLabel('把浏览器里的 code 贴回来').fill('new-code')
  await drawer.getByRole('button', { name: '换取凭证并更新此 key', exact: true }).click()
  await expect.poll(() => calls).toEqual([
    { provider: 'codex', channel_id: 42, channel_key_id: 7 },
    { state: 'targeted-state', code: 'new-code', channel_id: 42, channel_key_id: 7 },
  ])
})

test('手动刷新失败显示错误并刷新 key 观测', async ({ page }) => {
  const drawer = await openKey(page, 1)
  let count = 0
  page.on('request', (request) => { if (new URL(request.url()).pathname === '/admin/channels') count++ })
  await page.route('**/admin/channels/42/keys/7/oauth/refresh', async (route) => {
    expect(route.request().method()).toBe('POST')
    return route.fulfill({ status: 503, json: { error: { code: 'upstream_error', type: 'okapi_error', message: 'upstream_error' } } })
  })
  await drawer.getByRole('button', { name: '立即刷新凭证', exact: true }).click()
  await expect(page.getByRole('alert')).toBeVisible()
  await expect.poll(() => count).toBeGreaterThan(0)
  await expect(drawer.getByText('刷新失败：oauth_refresh_status_503')).toBeVisible()
})

test('预刷新设置校验边界并保存整数和停用开关', async ({ page }) => {
  await prepare(page)
  const defaults = { enabled: true, interval_secs: 30, refresh_margin_secs: 300, batch_size: 100, concurrency: 2, requests_per_second: 1 }
  let saved: unknown
  await page.route('**/admin/settings', async (route) => {
    if (route.request().isNavigationRequest()) return route.fallback()
    if (route.request().method() === 'POST') {
      saved = route.request().postDataJSON()
      return route.fulfill({ json: { ok: true } })
    }
    return route.fulfill({ json: { data: [
      { key: 'oauth_refresh_policy', value: defaults, configured: false, is_secret: false, updated_at: null },
    ] } })
  })
  await page.goto('/admin/settings')
  await page.getByRole('tab', { name: '高级设置' }).click()
  await page.getByRole('button', { name: '配置 订阅凭证预刷新' }).click()
  const drawer = page.getByRole('dialog')
  await drawer.getByLabel('并发刷新数量', { exact: true }).fill('9')
  await expect(drawer.getByRole('alert')).toContainText('必须为提示范围内的整数')
  await expect(drawer.getByRole('button', { name: '保存', exact: true })).toBeDisabled()
  await drawer.getByLabel('并发刷新数量', { exact: true }).fill('4')
  await drawer.getByRole('switch', { name: '启用后台预刷新' }).click()
  await drawer.getByRole('button', { name: '保存', exact: true }).click()
  await expect.poll(() => saved).toEqual({ key: 'oauth_refresh_policy', value: { ...defaults, concurrency: 4, enabled: false } })
})

test('直接导入 Claude token 并选择模拟请求风格，通过通用建渠道接口提交', async ({ page }) => {
  await prepare(page)
  let saved: Record<string, unknown> | undefined
  await page.route('**/admin/channels', async (route) => {
    if (route.request().isNavigationRequest()) return route.fallback()
    if (route.request().method() !== 'POST') return route.fallback()
    saved = route.request().postDataJSON()
    return route.fulfill({ json: { channel_id: 42, channel_key_id: 7 } })
  })
  await page.goto('/admin/channels')
  await page.getByRole('button', { name: '新建渠道' }).first().click()
  const drawer = page.getByRole('dialog')
  await expect(drawer.locator(':focus')).toHaveCount(1)
  await drawer.getByLabel('渠道名', { exact: true }).fill('direct-claude')
  await drawer.locator('#d-provider').selectOption('anthropic_max')
  await drawer.getByRole('tab', { name: '直接导入 Token', exact: true }).click()
  await drawer.getByLabel('上游凭证', { exact: true }).fill('sk-ant-oat01-ui-dummy')
  await expect(drawer.locator('#d-cred')).toHaveAttribute('type', 'password')
  await drawer.locator('#channel-client-options > summary').click()
  await expect(drawer.getByLabel('请求风格', { exact: true })).toHaveValue('mimic')
  await expect(drawer.getByLabel('客户端版本', { exact: true })).toHaveValue('2.1.290')
  await drawer.getByLabel('客户端入口', { exact: true }).selectOption('sdk-cli')
  await drawer.locator('#d-models').fill('claude-fixture')
  await drawer.locator('#d-models').press('Enter')
  await drawer.getByRole('button', { name: '新建', exact: true }).click()
  await expect.poll(() => saved).toMatchObject({
    provider: 'anthropic_max', credential: 'sk-ant-oat01-ui-dummy', models: ['claude-fixture'],
    settings: { extensions: { client_profile: { name: 'claude-code', mode: 'mimic', revision: '2.1.290', entrypoint: 'sdk-cli' } } },
  })
})

test('token-only 隐藏刷新按钮，多 key 替换显式选定且保存请求风格保留其他设置', async ({ page }) => {
  await prepare(page)
  const key = { status: 1, weight: 10, max_concurrency: null, cooldown_until: null,
    failed_count: 0, credential_kind: 1, oauth_refreshable: false }
  const settings = { oauth_token_url: 'https://fixture.example/token',
    extensions: { client_profile: { name: 'claude-code', mode: 'auto', revision: '2.1.290' } } }
  await page.route('**/admin/channels?*', (route) => route.fulfill({ json: {
    data: [{ id: 42, name: 'direct-token', provider: 'anthropic_max', api_base: '',
      status: 1, priority: 0, models: ['fixture-model'], settings, pools: ['default'], pool_members: [],
      cost_milli: 1000, data_retention: null, last_test: null, last_balance: null,
      keys: [{ ...key, id: 7 }, { ...key, id: 9 }],
    }], total: 1, enabled: 1,
  } }))
  let rotated: unknown, saved: Record<string, unknown> | undefined
  await page.route('**/admin/channels/42/credential', async (route) => {
    rotated = route.request().postDataJSON()
    return route.fulfill({ json: { ok: true, channel_key_id: 9 } })
  })
  await page.route('**/admin/channels/42', async (route) => {
    saved = route.request().postDataJSON()
    return route.fulfill({ json: { ok: true } })
  })
  await page.goto('/admin/channels')
  await page.getByRole('row').filter({ hasText: 'direct-token' }).getByRole('button', { name: '编辑', exact: true }).click()
  const drawer = page.getByRole('dialog')
  await expect(drawer.locator(':focus')).toHaveCount(1)
  await drawer.getByLabel('轮换凭证', { exact: true }).fill('sk-ant-oat01-replacement')
  const replace = drawer.getByRole('button', { name: '轮换', exact: true })
  await expect(replace).toBeDisabled()
  await drawer.getByLabel('选择要替换的 key', { exact: true }).selectOption('9')
  await replace.click()
  await expect.poll(() => rotated).toEqual({ credential: 'sk-ant-oat01-replacement', channel_key_id: 9 })
  await expect(drawer.locator('#d-cred')).toHaveValue('')
  await drawer.getByRole('tab', { name: '请求与计费行为', exact: true }).click()
  await drawer.locator('#channel-client-options > summary').click()
  await drawer.getByLabel('请求风格', { exact: true }).selectOption('mimic')
  await drawer.getByRole('button', { name: '保存', exact: true }).click()
  await expect.poll(() => saved).toMatchObject({ settings: {
    oauth_token_url: settings.oauth_token_url,
    extensions: { client_profile: { name: 'claude-code', mode: 'mimic', revision: '2.1.290' } },
  } })
  await drawer.getByRole('tab', { name: '接入信息', exact: true }).click()
  await expect(drawer.getByRole('button', { name: '立即刷新凭证', exact: true })).toHaveCount(0)
  await expect(drawer.getByText('仅 access token：', { exact: false })).toHaveCount(2)
})
