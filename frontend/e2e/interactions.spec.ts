import { expect, test } from '@playwright/test'
import type { Page } from '@playwright/test'
import { fileURLToPath } from 'node:url'
import { buildActivity } from '../src/features/profile/activity'
import type { ActivityResponse } from '../src/features/profile/activity'

function profileData(year = 2026, scope: 'key' | 'user' = 'key'): ActivityResponse {
  const row = (day: string, n = 1) => ({ day, model: 'gpt-5', requests: 2 * n,
    prompt_tokens: 1000 * n, completion_tokens: 500 * n, cached_tokens: 400 * n,
    reasoning_tokens: 200 * n, amount_micro: 12500 * n, discount_micro: 0, errors: 0 })
  const data = year === 2024 ? [row('2024-02-28'), row('2024-02-29')]
    : year === 2025 ? [] : [row('2026-01-01'), { ...row('2026-01-01'), model: 'claude-sonnet-4', prompt_tokens: 300,
      completion_tokens: 200, cached_tokens: 100, reasoning_tokens: 100, requests: 3, amount_micro: 2500 }]
  if (year === 2026) {
    for (let i = 2; i <= 246; i++) {
      if (i % 7 === 0 || i % 11 === 0) continue
      const day = new Date(Date.UTC(year, 0, i)).toISOString().slice(0, 10)
      data.push(row(day, (i * 13) % 21 + 1))
    }
    data.push({ ...row('2026-09-04'), prompt_tokens: 0, completion_tokens: 0, cached_tokens: 0, reasoning_tokens: 0, requests: 1, amount_micro: 0, errors: 1 })
  }
  return { year, scope, today: '2026-09-04', timezone: 'UTC', first_year: 2024,
    data: scope === 'user' ? data.map((r) => ({ ...r, requests: r.requests * 2 })) : data }
}

async function prepareProfile(page: Page, language = 'zh-CN') {
  await prepare(page, [], language)
  const queries: string[] = []
  await page.route('**/api/me/stats/activity?*', async (route) => {
    const url = new URL(route.request().url())
    queries.push(url.search)
    await route.fulfill({ json: profileData(Number(url.searchParams.get('year') ?? 2026), url.searchParams.get('scope') === 'user' ? 'user' : 'key') })
  })
  return queries
}

test('年度日历覆盖闰日，按输入加输出汇总，连续天数跨月且不计未来记录', () => {
  const data = profileData(2024)
  data.data.push({ ...data.data[0], day: '2024-03-01' })
  const result = buildActivity(data)
  expect(result.days).toHaveLength(366)
  expect(result.total.tokens).toBe(4500)
  expect(result.total.activeDays).toBe(3)
  expect(result.longestStreak).toBe(3)
  expect(result.lookup.get('2024-02-29')?.tokens).toBe(1500)
  const current = profileData()
  const before = buildActivity(current).total.tokens
  current.data.push({ ...current.data[0], day: '2026-12-31' })
  expect(buildActivity(current).total.tokens).toBe(before)
  expect(buildActivity(current).days).toHaveLength(365)
})

test('底部头像进入个人中心，热力图可点击、键盘切日并切换指标', async ({ page }) => {
  const queries = await prepareProfile(page)
  await page.goto('/portal/keys')
  await page.getByRole('complementary').getByRole('link', { name: '个人中心', exact: true }).click()
  await expect(page).toHaveURL(/\/portal\/profile$/)
  await expect(page.getByRole('main').getByRole('heading', { name: '个人中心', exact: true })).toBeVisible()
  const cells = page.locator('button[data-day]')
  await expect(cells).toHaveCount(247)
  await expect(page.locator('[data-future]')).toHaveCount(118)
  const cellSize = await cells.first().boundingBox()
  expect(cellSize?.width).toBe(cellSize?.height)
  await page.locator('[data-day="2026-01-01"]').click()
  const details = page.getByRole('region', { name: '当天明细', exact: true })
  await expect(details.getByLabel('选择日期')).toHaveValue('2026-01-01')
  await expect(details.locator('dl')).toContainText('2,000')
  await expect(details.locator('tbody tr')).toHaveCount(2)
  await expect(details.locator('tbody tr').first()).toContainText('1,500')
  await page.locator('[data-day="2026-01-01"]').press('ArrowRight')
  await expect(page.locator('[data-day="2026-01-08"]')).toBeFocused()
  await expect(details.getByLabel('选择日期')).toHaveValue('2026-01-08')
  await page.locator('[data-day="2026-01-08"]').press('Home')
  await expect(page.locator('[data-day="2026-01-01"]')).toBeFocused()
  await expect(page.locator('[data-day="2026-09-04"]')).toHaveAttribute('data-level', '0')
  await page.getByRole('group', { name: '热力图指标' }).getByRole('button', { name: '请求数', exact: true }).click()
  await expect(page.locator('[data-day="2026-09-04"]')).not.toHaveAttribute('data-level', '0')
  expect(queries).toEqual(['?scope=key'])
  await page.getByLabel('选择日期').fill('2026-09-03')
  await page.getByRole('group', { name: '热力图指标' }).getByRole('button', { name: 'Token', exact: true }).click()
  await page.evaluate(() => window.scrollTo(0, 0))
  await page.screenshot({ path: 'test-results/personal-center-desktop.png', fullPage: true, animations: 'disabled' })
})

test('年份与账户范围分别查询，空年和空日可辨识', async ({ page }) => {
  const queries = await prepareProfile(page)
  await page.goto('/portal/profile')
  await page.getByLabel('年份', { exact: true }).selectOption('2024')
  await expect(page.locator('button[data-day]')).toHaveCount(366)
  await page.getByLabel('选择日期').fill('2024-02-29')
  await expect(page.getByRole('region', { name: '当天明细', exact: true })).toContainText('1,500')
  await page.getByRole('group', { name: '统计范围' }).getByRole('button', { name: '全账户' }).click()
  await expect.poll(() => queries.at(-1)).toBe('?scope=user&year=2024')
  await expect(page.locator('[data-day="2024-02-29"]')).toHaveAttribute('aria-label', /4 次请求/)
  await page.getByLabel('选择日期').fill('2024-01-01')
  await expect(page.getByText('当天暂无使用记录', { exact: true })).toBeVisible()
  await page.getByLabel('年份', { exact: true }).selectOption('2025')
  await expect(page.getByText(/该年份和统计范围内暂无使用记录/)).toBeVisible()
  await expect(page.locator('button[data-day]')).toHaveCount(365)
})

test('移动端头像入口关闭菜单，日历不撑宽页面，日期选择与深色模式可用', async ({ page }) => {
  await prepareProfile(page)
  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto('/portal/keys')
  await page.getByRole('button', { name: '打开导航' }).click()
  await page.getByRole('dialog').getByRole('link', { name: '个人中心', exact: true }).click()
  await expect(page.getByRole('dialog')).toHaveCount(0)
  await page.getByLabel('选择日期').fill('2026-01-01')
  await expect(page.getByRole('region', { name: '当天明细', exact: true })).toContainText('2,000')
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  await page.evaluate(() => window.scrollTo(0, 0))
  await page.screenshot({ path: 'test-results/personal-center-mobile.png', fullPage: true, animations: 'disabled' })
  await page.setViewportSize({ width: 1440, height: 1000 })
  await page.evaluate(() => document.documentElement.classList.add('dark'))
  await page.getByRole('button', { name: '收起侧栏' }).click()
  await expect(page.getByRole('complementary').getByRole('link', { name: '个人中心', exact: true })).toBeVisible()
  await page.evaluate(() => window.scrollTo(0, 0))
  await page.screenshot({ path: 'test-results/personal-center-dark.png', fullPage: true, animations: 'disabled' })
})

test('统计服务不可用时显示错误与重试，不伪装成零用量', async ({ page }) => {
  await prepareProfile(page, 'en')
  await page.route('**/api/me/stats/activity?*', (route) => route.fulfill({ status: 501, json: { error: { code: 'stats_disabled' } } }))
  await page.goto('/portal/profile')
  await expect(page.getByRole('alert')).toBeVisible()
  await expect(page.locator('[data-day]')).toHaveCount(0)
  await expect(page.getByText('Total tokens', { exact: true })).toHaveCount(0)
  await page.unroute('**/api/me/stats/activity?*')
  await page.route('**/api/me/stats/activity?*', (route) => route.fulfill({ json: profileData() }))
  await page.getByRole('button', { name: 'Retry' }).click()
  await expect(page.getByRole('group', { name: 'Daily usage heatmap' })).toBeVisible()
})

async function prepare(page: Page, permissions = ['*'], language = 'en') {
  const requests: string[] = []
  await page.addInitScript((lang) => {
    localStorage.setItem('okapi.key', 'interaction-test-key')
    localStorage.setItem('okapi.lang', lang)
  }, language)
  await page.route('**/*', async (route) => {
    const request = route.request()
    const path = new URL(request.url()).pathname
    if (request.isNavigationRequest()) {
      // UI 深链由 SPA 处理，避免 Vite 的 /admin 开发代理转发到真实 console。
      await route.fulfill({ path: fileURLToPath(new URL('../dist/index.html', import.meta.url)), contentType: 'text/html' })
    } else if (/^\/(api|admin|auth|pay)\//.test(path)) {
      requests.push(path)
      expect(request.method(), '交互回归不应提交业务修改').toBe('GET')
      const json = path === '/api/me' ? {
        user_id: 1, key_id: 1, group: 'default', balance_micro: 10000000,
        balance_expires_at: null, role: permissions.length ? 100 : 1, permissions,
      } : path === '/api/notice' ? { notice: null }
        : path.startsWith('/admin/settings/') ? { value: null }
        : { data: [], next_before: null }
      await route.fulfill({ json })
    } else {
      await route.continue()
    }
  })
  return requests
}

async function advancedSettings(page: Page, permissions = ['*']) {
  await prepare(page, permissions, 'zh-CN')
  const values: Record<string, unknown> = {
    aff_percent_bp: 1000,
    epay_key_test: null,
    payment_epay: { gateway_url: 'https://pay.example.test/submit.php', pid: '1001', key: 'test-epay-secret', usd_to_cny_milli: 7000, custom: { preserve: true } },
    payment_stripe: { secret_key: 'test-stripe-secret', webhook_secret: 'test-webhook-secret', api_base: 'https://api.stripe.com' },
    oauth_providers: [{ code: 'github', client_id: 'client-1', client_secret: 'test-oauth-secret', custom: 'keep' }],
    notify_channels: [{ type: 'webhook', url: 'https://example.test/hooks?token=test-notify-secret', events: ['drift'], min_interval_secs: 60 }],
    model_rpm_limits: { 'model-a': 2 },
    mcp_write_enabled: true,
    ssrf_policy: { allow_http: true, allow_private: true, custom: 'keep' },
    extension_custom: { retries: 3, nested: { password: 'test-extension-secret' } },
  }
  const writes: { key: string; value: unknown }[] = []
  await page.route('**/admin/settings', async (route) => {
    if (route.request().isNavigationRequest()) return route.fallback()
    if (route.request().method() === 'POST') {
      const body = route.request().postDataJSON()
      writes.push(body)
      values[body.key] = body.value
      await route.fulfill({ json: { ok: true } })
    } else {
      await route.fulfill({ json: { data: Object.entries(values).map(([key, value]) => ({ key, value, is_secret: key === 'epay_key_test', configured: true, updated_at: '2026-09-04T09:02:00Z' })) } })
    }
  })
  await page.goto('/admin/settings')
  await page.getByRole('tab', { name: '高级设置' }).click()
  await expect(page.getByRole('article', { name: '易支付', exact: true })).toBeVisible()
  return { writes, values }
}

test('高级配置按用途分组，支持中文搜索，列表与悬浮信息不显示敏感值', async ({ page }) => {
  await advancedSettings(page)
  const panel = page.getByRole('tabpanel')
  await expect(panel.getByRole('article')).toHaveCount(10)
  await expect(panel.getByRole('article', { name: '充值返利', exact: true })).toContainText('10%')
  for (const secret of ['test-epay-secret', 'test-stripe-secret', 'test-webhook-secret', 'test-oauth-secret', 'test-notify-secret', 'test-extension-secret']) {
    expect(await panel.innerHTML()).not.toContain(secret)
  }
  const search = panel.getByRole('searchbox')
  await search.fill('返利')
  await expect(panel.getByRole('article')).toHaveCount(4)
  await search.fill('模型请求')
  await expect(panel.getByRole('article')).toHaveCount(1)
  await search.fill('no-match')
  await expect(panel.getByRole('article')).toHaveCount(0)
  await panel.getByRole('button', { name: '清除筛选' }).click()
  await panel.getByRole('group', { name: '配置分类' }).getByRole('button', { name: /访问与安全/ }).click()
  await expect(panel.getByRole('article')).toHaveCount(2)
  await panel.getByRole('button', { name: '清除筛选' }).click()
  await page.screenshot({ path: 'test-results/advanced-settings-desktop.png', fullPage: true, animations: 'disabled' })
  await page.setViewportSize({ width: 390, height: 844 })
  await expect.poll(async () => {
    const tab = await page.getByRole('tab', { name: '高级设置' }).boundingBox()
    const list = await page.getByRole('tablist').boundingBox()
    return !!tab && !!list && tab.x >= list.x && tab.x + tab.width <= list.x + list.width + 1
  }).toBe(true)
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  await page.screenshot({ path: 'test-results/advanced-settings-mobile.png', fullPage: true, animations: 'disabled' })
})

test('返利与汇率按易读单位编辑，非法数值不能提交，保留密钥和未知字段', async ({ page }) => {
  const { writes, values } = await advancedSettings(page)
  await page.getByRole('article', { name: '充值返利', exact: true }).getByRole('button').click()
  let dialog = page.getByRole('dialog', { name: '充值返利', exact: true })
  const percent = dialog.getByLabel('返利比例')
  await expect(percent).toHaveValue('10')
  await expect(dialog.getByRole('button', { name: '保存', exact: true })).toBeDisabled()
  for (const invalid of ['', 'abc', '-1', '100.01', '1.234']) {
    await percent.fill(invalid)
    await expect(dialog.getByRole('button', { name: '保存', exact: true })).toBeDisabled()
  }
  expect(writes).toHaveLength(0)
  await percent.fill('12.35')
  await dialog.getByRole('button', { name: '保存', exact: true }).click()
  await expect(dialog).toHaveCount(0)
  expect(values.aff_percent_bp).toBe(1235)
  await expect(page.getByRole('article', { name: '充值返利', exact: true })).toContainText('12.35%')
  await page.getByRole('article', { name: '易支付', exact: true }).getByRole('button').click()
  dialog = page.getByRole('dialog', { name: '易支付', exact: true })
  await expect(dialog.getByLabel('商户密钥')).toHaveAttribute('type', 'password')
  await expect(dialog.getByLabel('人民币汇率（1 美元兑换）')).toHaveValue('7')
  await dialog.getByLabel('人民币汇率（1 美元兑换）').fill('7.123')
  await dialog.getByLabel('商户 ID').fill('1002')
  await page.screenshot({ path: 'test-results/advanced-settings-editor.png', animations: 'disabled' })
  await dialog.getByRole('button', { name: '保存', exact: true }).click()
  await expect(dialog).toHaveCount(0)
  expect(values.payment_epay).toMatchObject({ pid: '1002', key: 'test-epay-secret', usd_to_cny_milli: 7123, custom: { preserve: true } })
})

test('模型规则防重复，零值不限流，访问开关保留扩展字段', async ({ page }) => {
  const { values } = await advancedSettings(page)
  await page.getByRole('article', { name: '模型请求限流' }).getByRole('button').click()
  let dialog = page.getByRole('dialog', { name: '模型请求限流' })
  await dialog.getByRole('button', { name: '添加限流规则' }).click()
  await dialog.getByLabel('模型名称', { exact: true }).last().fill('model-a')
  await dialog.getByLabel('RPM', { exact: true }).last().fill('3')
  await expect(dialog.getByRole('button', { name: '保存', exact: true })).toBeDisabled()
  await dialog.getByLabel('模型名称', { exact: true }).last().fill('model-b')
  await dialog.getByLabel('RPM', { exact: true }).last().fill('0')
  await dialog.getByRole('button', { name: '保存', exact: true }).click()
  await expect(dialog).toHaveCount(0)
  expect(values.model_rpm_limits).toEqual({ 'model-a': 2, 'model-b': 0 })
  await page.getByRole('article', { name: '上游访问策略' }).getByRole('button').click()
  dialog = page.getByRole('dialog', { name: '上游访问策略' })
  await dialog.getByRole('switch', { name: '允许 HTTP 上游', exact: true }).click()
  await dialog.getByRole('button', { name: '保存', exact: true }).click()
  await expect(dialog).toHaveCount(0)
  expect(values.ssrf_policy).toEqual({ allow_http: false, allow_private: true, custom: 'keep' })
})

test('登录服务商保留密钥，扩展 JSON 明确展开并校验，通知入口跳转专用表单', async ({ page }) => {
  const { values } = await advancedSettings(page)
  await page.getByRole('article', { name: '第三方登录' }).getByRole('button').click()
  let dialog = page.getByRole('dialog', { name: '第三方登录' })
  await expect(dialog.getByLabel('客户端密钥')).toHaveAttribute('type', 'password')
  await dialog.getByLabel('客户端 ID').fill('client-new')
  await dialog.getByRole('button', { name: '保存', exact: true }).click()
  await expect(dialog).toHaveCount(0)
  expect(values.oauth_providers).toEqual([{ code: 'github', client_id: 'client-new', client_secret: 'test-oauth-secret', custom: 'keep' }])
  await page.getByRole('article', { name: 'extension_custom' }).getByRole('button').click()
  dialog = page.getByRole('dialog', { name: 'extension_custom' })
  await expect(dialog.locator('textarea')).toHaveCount(0)
  await dialog.getByRole('button', { name: '显示并编辑敏感配置' }).click()
  await dialog.locator('textarea').fill('{ broken')
  await expect(dialog.getByRole('button', { name: '保存', exact: true })).toBeDisabled()
  await expect(dialog.getByRole('alert')).toContainText('JSON 格式错误')
  await dialog.getByRole('button', { name: '取消', exact: true }).click()
  expect(values.extension_custom).toEqual({ retries: 3, nested: { password: 'test-extension-secret' } })
  await page.getByRole('article', { name: '通知多路' }).getByRole('button').click()
  await expect(page.getByRole('tab', { name: '通知多路' })).toHaveAttribute('aria-selected', 'true')
  await expect(page.getByRole('tab', { name: '通知多路' })).toBeFocused()
})

test('只读权限可以浏览高级配置但不会出现编辑入口', async ({ page }) => {
  const { writes } = await advancedSettings(page, ['settings.read'])
  await expect(page.getByRole('tabpanel').getByRole('article')).toHaveCount(10)
  await expect(page.getByRole('tabpanel').getByRole('article').getByRole('button')).toHaveCount(0)
  expect(writes).toHaveLength(0)
})

test('门户按任务分组，搜索支持分组、空态、清空和回车跳转', async ({ page }) => {
  await prepare(page)
  await page.goto('/portal/ledger')
  const nav = page.getByRole('navigation', { name: 'Navigation', exact: true })
  const usage = nav.locator('div').filter({ has: page.getByText('API & usage', { exact: true }) })
  await expect(usage.getByRole('link')).toHaveCount(3)
  expect(await usage.locator('a').evaluateAll((links) => links.map((link) => link.getAttribute('href'))))
    .toEqual(['/portal/keys', '/pricing', '/portal/logs'])
  const search = page.getByRole('searchbox', { name: 'Find a feature' })
  await search.fill('billing')
  await expect(nav.getByRole('link')).toHaveCount(3)
  await search.fill('no-such-feature')
  await expect(nav.getByRole('status')).toContainText('No matching features')
  await search.press('Escape')
  await expect(search).toHaveValue('')
  await search.fill('keys')
  await page.getByRole('button', { name: 'Clear', exact: true }).click()
  await expect(search).toBeFocused()
  await search.fill('keys')
  await search.press('Enter')
  await expect(page).toHaveURL(/\/portal\/keys$/)
  await expect(search).toHaveValue('')
  await expect(nav.locator('[aria-current=page]')).toHaveAttribute('href', '/portal/keys')
})

test('无管理权限不显示工作区入口，搜索也不泄漏受限功能', async ({ page }) => {
  await prepare(page, [])
  await page.goto('/portal/ledger')
  await expect(page.getByRole('complementary').locator('a[href="/admin"]')).toHaveCount(0)
  await page.goto('/admin/settings')
  await page.getByRole('searchbox', { name: 'Find a feature' }).fill('channels')
  await expect(page.getByRole('navigation', { name: 'Navigation', exact: true }).getByRole('link')).toHaveCount(0)
  // 返回门户固定在搜索区外，零结果时仍然可以离开。
  await expect(page.getByRole('complementary').locator('a[href="/portal"]')).toBeVisible()
})

test('图标侧栏仍有可访问名称，搜索可展开并定位输入框，折叠偏好保留', async ({ page }) => {
  await prepare(page)
  await page.goto('/portal/ledger')
  await page.getByRole('button', { name: 'Collapse sidebar' }).click()
  await expect(page.getByRole('navigation').locator('a[href="/portal/keys"]')).toHaveAccessibleName(/.+/)
  await page.reload()
  await expect(page.getByRole('button', { name: 'Expand sidebar' })).toBeVisible()
  await page.getByRole('button', { name: 'Find a feature' }).click()
  await expect(page.getByRole('searchbox', { name: 'Find a feature' })).toBeFocused()
})

test('手机导航锁定背景、循环焦点，Esc 和当前页链接均能关闭并恢复焦点', async ({ page }) => {
  await prepare(page)
  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto('/portal/ledger')
  const opener = page.getByRole('button', { name: 'Open navigation' })
  await expect(page.locator('#app-navigation')).toHaveAttribute('inert', '')
  await opener.click()
  const dialog = page.getByRole('dialog', { name: 'Navigation', exact: true })
  await expect(dialog).toBeVisible()
  await expect(page.locator('main').locator('..')).toHaveAttribute('inert', '')
  await expect(page.locator('body')).toHaveCSS('overflow', 'hidden')
  const first = dialog.locator('a').first()
  await expect(first).toBeFocused()
  await page.screenshot({ path: 'test-results/interaction-mobile-nav.png', animations: 'disabled' })
  await first.press('Shift+Tab')
  await expect(dialog.locator('a').last()).toBeFocused()
  await page.keyboard.press('Tab')
  await expect(first).toBeFocused()
  const search = dialog.getByRole('searchbox')
  await search.fill('missing')
  await search.press('Escape')
  await expect(dialog).toBeVisible()
  await expect(search).toHaveValue('')
  await search.press('Escape')
  await expect(dialog).toHaveCount(0)
  await expect(opener).toBeFocused()
  await expect(page.locator('body')).not.toHaveCSS('overflow', 'hidden')
  await opener.click()
  await dialog.locator('a[href="/portal/ledger"]').click()
  await expect(dialog).toHaveCount(0)
  await expect(opener).toBeFocused()
})

test('菜单支持方向键、首尾、确认、Esc、Tab 和外点退出', async ({ page }) => {
  await prepare(page)
  await page.goto('/portal/ledger')
  const trigger = page.getByRole('button', { name: 'Theme', exact: true })
  await trigger.focus()
  await trigger.press('ArrowUp')
  const menu = page.getByRole('menu', { name: 'Theme', exact: true })
  await expect(menu.getByRole('menuitem').last()).toBeFocused()
  await page.keyboard.press('ArrowDown')
  await expect(menu.getByRole('menuitem').first()).toBeFocused()
  await page.keyboard.press('End')
  await page.keyboard.press('ArrowUp')
  await page.keyboard.press('Enter')
  await expect(page.locator('html')).toHaveClass(/dark/)
  await expect(trigger).toBeFocused()
  await expect(trigger).toHaveAttribute('aria-expanded', 'false')
  await trigger.press('ArrowDown')
  await page.keyboard.press('Escape')
  await expect(trigger).toBeFocused()
  await trigger.click()
  await page.keyboard.press('Tab')
  await expect(menu).toHaveCount(0)
  await expect(page.getByRole('button', { name: 'Sign out', exact: true })).toBeFocused()
  await trigger.click()
  await page.keyboard.press('Shift+Tab')
  await expect(trigger).toBeFocused()
  await expect(menu).toHaveCount(0)
  await trigger.click()
  await page.getByRole('main').click({ position: { x: 10, y: 10 } })
  await expect(menu).toHaveCount(0)
})

test('页签方向键只移动焦点，确认后才请求数据；离开后从选中项重入', async ({ page }) => {
  const requests = await prepare(page)
  await page.goto('/portal/ledger')
  const tabs = page.getByRole('tab')
  await expect(tabs.first()).toHaveAttribute('aria-selected', 'true')
  await tabs.first().focus()
  const before = requests.filter((path) => path.includes('orders')).length
  await tabs.first().press('ArrowRight')
  await expect(tabs.last()).toBeFocused()
  await expect(tabs.first()).toHaveAttribute('aria-selected', 'true')
  expect(requests.filter((path) => path.includes('orders'))).toHaveLength(before)
  await page.keyboard.press('Enter')
  await expect(tabs.last()).toHaveAttribute('aria-selected', 'true')
  await expect.poll(() => requests.filter((path) => path.includes('orders')).length).toBeGreaterThan(before)
  await page.keyboard.press('Home')
  await expect(tabs.first()).toBeFocused()
  await page.keyboard.press('ArrowLeft')
  await expect(tabs.last()).toBeFocused()
  await page.getByRole('button', { name: 'Theme', exact: true }).focus()
  await expect(tabs.last()).toHaveAttribute('tabindex', '0')
  await expect(tabs.first()).toHaveAttribute('tabindex', '-1')
})

test('设置常用表单优先，高级区延迟加载，切签保留未保存内容', async ({ page }) => {
  const requests = await prepare(page)
  await page.goto('/admin/settings')
  const tabs = page.getByRole('tablist')
  await expect(tabs.getByRole('tab').first()).toHaveAccessibleName('Registration & risk')
  await expect(tabs.getByRole('tab').last()).toHaveAccessibleName('Advanced settings')
  await expect(page.getByRole('tabpanel', { name: 'Registration & risk' })).toBeVisible()
  const nav = page.getByRole('navigation', { name: 'Navigation', exact: true })
  const current = nav.locator('[aria-current=page]')
  await expect.poll(async () => {
    const bounds = await nav.boundingBox()
    const rect = await current.boundingBox()
    return !!bounds && !!rect && rect.y >= bounds.y && rect.y + rect.height <= bounds.y + bounds.height + 1
  }).toBe(true)
  await page.screenshot({ path: 'test-results/interaction-desktop.png', animations: 'disabled' })
  await page.locator('#reg-gift').fill('12.5')
  expect(requests).not.toContain('/admin/settings')
  await tabs.getByRole('tab', { name: 'Site notice' }).click()
  await page.locator('#notice-title').fill('Draft maintenance notice')
  await tabs.getByRole('tab', { name: 'Registration & risk' }).click()
  await expect(page.locator('#reg-gift')).toHaveValue('12.5')
  await tabs.getByRole('tab', { name: 'Site notice' }).click()
  await expect(page.locator('#notice-title')).toHaveValue('Draft maintenance notice')
  await tabs.getByRole('tab', { name: 'Advanced settings' }).click()
  await expect.poll(() => requests.includes('/admin/settings')).toBe(true)
  await expect(page.getByRole('tabpanel')).toHaveCount(1)
})

test('高级配置跳转专用表单，保存后刷新摘要并保留后续草稿', async ({ page }) => {
  await prepare(page)
  let credit = 0
  let listReads = 0
  // 此例的写操作也完全在内存接口桩中完成。
  await page.route('**/admin/settings/registration_policy', (route) => route.fulfill({
    json: { value: { new_user_credit_micro: credit } },
  }))
  await page.route('**/admin/settings', async (route) => {
    if (route.request().isNavigationRequest()) return route.fallback()
    if (route.request().method() === 'POST') {
      credit = route.request().postDataJSON().value.new_user_credit_micro
      await route.fulfill({ json: { ok: true } })
    } else {
      listReads++
      await route.fulfill({ json: { data: [{ key: 'registration_policy', value: { new_user_credit_micro: credit }, is_secret: false, updated_at: null }] } })
    }
  })
  await page.goto('/admin/settings')
  await page.getByRole('tab', { name: 'Advanced settings' }).click()
  await page.getByRole('article', { name: 'Registration & risk' }).getByRole('button').click()
  await expect(page.getByRole('tab', { name: 'Registration & risk' })).toHaveAttribute('aria-selected', 'true')
  const initialReads = listReads
  await page.locator('#reg-gift').fill('12.5')
  await page.getByRole('tabpanel').getByRole('button', { name: 'Save', exact: true }).click()
  await expect.poll(() => credit).toBe(12500000)
  await expect(page.locator('#reg-gift')).toHaveValue('12.5')
  await expect.poll(() => listReads).toBeGreaterThan(initialReads)
  await page.locator('#reg-gift').fill('25')
  await page.getByRole('tab', { name: 'Advanced settings' }).click()
  await page.getByRole('article', { name: 'Registration & risk' }).getByRole('button').click()
  await expect(page.locator('#reg-gift')).toHaveValue('25')
})

test('中文输入法确认不触发搜索跳转，窄屏页签可定位且页面不横向溢出', async ({ page }) => {
  await prepare(page, ['*'], 'zh-CN')
  await page.setViewportSize({ width: 320, height: 740 })
  await page.goto('/admin/settings')
  await page.getByRole('button', { name: '打开导航' }).click()
  const search = page.getByRole('searchbox', { name: '搜索功能' })
  await search.fill('settings')
  await search.dispatchEvent('keydown', { key: 'Enter', code: 'Enter', isComposing: true, bubbles: true })
  await expect(page.getByRole('dialog')).toBeVisible()
  await expect(search).toHaveValue('settings')
  await search.press('Escape')
  await search.press('Escape')
  const tabs = page.getByRole('tablist')
  await tabs.getByRole('tab').first().focus()
  await page.keyboard.press('End')
  await expect(tabs.getByRole('tab').last()).toBeFocused()
  const last = await tabs.getByRole('tab').last().boundingBox()
  const list = await tabs.boundingBox()
  expect(last!.x + last!.width).toBeLessThanOrEqual(list!.x + list!.width + 1)
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  await page.screenshot({ path: 'test-results/interaction-mobile.png', fullPage: true, animations: 'disabled' })
})
