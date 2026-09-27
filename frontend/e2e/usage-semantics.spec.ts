import { expect, test } from '@playwright/test'
import type { Page } from '@playwright/test'
import { fileURLToPath } from 'node:url'
import { overviewSearch } from '../src/features/portal-overview/search'
import { portalLogSearch } from '../src/features/logs/search'
import { sumByModel } from '../src/features/portal-overview/types'

const usageRow = { day: '2026-09-27', model: 'model-a', requests: 2, prompt_tokens: 1000, cached_tokens: 0,
  cache_read_known_requests: 2, cache_write_known_requests: 2, cache_write_tokens: 0, completion_tokens: 100,
  reasoning_tokens: 0, amount_micro: 10000, discount_micro: 500, errors: 0 }
const report = () => ({ scope: 'user', days: 7, live: null, data: [usageRow], total: { ...usageRow,
  tokens: 1100, cache_hit_bp: 0, avg_rpm_micro: 100, avg_tpm_micro: 1000,
  avg_ttft_ms: 150, ttft_samples: 1, avg_latency_ms: 1000, success_rate_bp: 10000, tokens_per_1k_sec: 50000 } })

async function prepare(page: Page, mode: 'account' | 'key' | 'legacy' | 'signed-out' = 'account') {
  const requests: URL[] = []
  await page.addInitScript((mode) => {
    localStorage.setItem('okapi.lang', 'zh-CN')
    if (mode !== 'signed-out') localStorage.setItem('okapi.key', 'usage-fixture')
    if (mode === 'account' || mode === 'key') localStorage.setItem('okapi.login-mode', mode)
    localStorage.setItem('okapi.guide.1', 'dismissed')
  }, mode)
  await page.route('**/*', (route) => {
    const request = route.request(), url = new URL(request.url())
    if (request.isNavigationRequest()) return route.fulfill({ path: fileURLToPath(new URL('../dist/index.html', import.meta.url)), contentType: 'text/html' })
    if (!/^\/(api|auth)\//.test(url.pathname)) return route.continue()
    requests.push(url)
    const responses: Record<string, unknown> = {
      '/api/me': { user_id: 1, key_id: 2, key_name: 'work-key', key_prefix: 'sk-prefix', has_web_session: true, group: 'default', role: 1, permissions: [], balance_micro: 10000000, subscription_remaining_micro: 0 },
      '/api/notice': { notice: null }, '/api/pricing': { models: [], groups: [] },
      '/api/me/stats/breakdown': report(), '/api/me/logs': { data: [], next_before: null },
      '/api/me/stats/activity': { year: 2026, today: '2026-09-27', first_year: 2026, timezone: 'UTC', scope: url.searchParams.get('scope'), data: [] },
      '/api/registration': { mode: 'open', email_verification: false, new_user_credit_micro: 0 },
      '/auth/oauth-providers': { providers: [] }, '/auth/login': { ok: true }, '/auth/keys': { api_key: 'web-fixture' },
    }
    return route.fulfill({ json: responses[url.pathname] ?? { data: [] } })
  })
  return requests
}

test('明确 key 范围保留在 URL，模型缓存覆盖按请求数累加', () => {
  expect(overviewSearch({ scope: 'key' }).scope).toBe('key')
  expect(portalLogSearch({ scope: 'key' }).scope).toBe('key')
  const row = sumByModel([usageRow, { ...usageRow, cache_read_known_requests: 0 }]).get('model-a')!
  expect(row.requests).toBe(4)
  expect(row.cache_read_known_requests).toBe(2)
})

for (const mode of ['account', 'legacy'] as const) {
  test(`${mode} 账号登录：总览、日志、个人页都看账户，不暴露登录凭证的空统计`, async ({ page }) => {
    const requests = await prepare(page, mode)
    for (const [path, endpoint] of [['/portal?scope=key', '/api/me/stats/breakdown'], ['/portal/logs?scope=key', '/api/me/logs'], ['/portal/profile', '/api/me/stats/activity']]) {
      await page.goto(path)
      await expect(page.getByText('账户全部密钥', { exact: true })).toBeVisible()
      await expect(page.getByRole('button', { name: '本密钥', exact: true })).toHaveCount(0)
      await expect.poll(() => requests.filter((r) => r.pathname === endpoint).at(-1)?.searchParams.get('scope')).toBe('user')
    }
    expect(requests.filter((r) => r.pathname.startsWith('/api/me/stats/') || r.pathname === '/api/me/logs').every((r) => r.searchParams.get('scope') === 'user')).toBe(true)
  })
}

test('API Key 登录仍默认本密钥，即使浏览器保留同账户会话；切换后跨页沿用并可显式切回', async ({ page }) => {
  const requests = await prepare(page, 'key')
  await page.goto('/portal')
  await expect(page.getByRole('button', { name: '本密钥', exact: true })).toHaveAttribute('aria-pressed', 'true')
  await expect(page.getByText('work-key · sk-prefix', { exact: true })).toBeVisible()
  await page.getByRole('button', { name: '全账户', exact: true }).click()
  await page.getByRole('link', { name: '调用明细', exact: true }).click()
  await expect.poll(() => requests.filter((r) => r.pathname === '/api/me/logs').at(-1)?.searchParams.get('scope')).toBe('user')
  await page.goto('/portal/profile')
  await expect.poll(() => requests.filter((r) => r.pathname === '/api/me/stats/activity').at(-1)?.searchParams.get('scope')).toBe('user')
  await page.goto('/portal?scope=key')
  await expect(page.getByRole('button', { name: '本密钥', exact: true })).toHaveAttribute('aria-pressed', 'true')
  await expect.poll(() => requests.filter((r) => r.pathname === '/api/me/stats/breakdown').at(-1)?.searchParams.get('scope')).toBe('key')
})

test('邮箱登录与 OAuth 兑换记录账户模式', async ({ page }) => {
  await prepare(page, 'signed-out')
  await page.goto('/')
  await page.getByLabel('邮箱', { exact: true }).fill('fixture@example.test')
  await page.getByLabel('密码', { exact: true }).fill('fixture-password')
  await page.getByRole('button', { name: '登录', exact: true }).click()
  await expect(page.getByText('账户全部密钥', { exact: true })).toBeVisible()
  expect(await page.evaluate(() => localStorage.getItem('okapi.login-mode'))).toBe('account')
  await page.evaluate(() => { localStorage.removeItem('okapi.key'); localStorage.removeItem('okapi.login-mode') })
  await page.goto('/?oauth=done')
  await expect(page.getByText('账户全部密钥', { exact: true })).toBeVisible()
  expect(await page.evaluate(() => localStorage.getItem('okapi.login-mode'))).toBe('account')
})

test('首字延迟常驻显示，有样本显示毫秒，无样本和错误分别解释', async ({ page }) => {
  await prepare(page)
  await page.setViewportSize({ width: 1440, height: 1000 })
  await page.goto('/portal')
  const metrics = page.getByRole('region', { name: '调用质量', exact: true })
  await expect(metrics).toContainText('首字延迟（TTFT）')
  await expect(metrics).toContainText('150 ms')
  await expect(metrics).toContainText('1 个已采集首字样本')
  await page.screenshot({ path: 'test-results/usage-semantics-desktop.png', fullPage: true, animations: 'disabled' })
  const empty = report(); empty.total.avg_ttft_ms = null as unknown as number; empty.total.ttft_samples = 0
  await page.route('**/api/me/stats/breakdown?*', (route) => route.fulfill({ json: empty }))
  await page.getByRole('button', { name: '刷新', exact: true }).click()
  await expect(metrics).toContainText('暂无首字样本')
  await page.route('**/api/me/stats/breakdown?*', (route) => route.fulfill({ status: 500, json: { error: { code: 'internal_error' } } }))
  await page.getByRole('button', { name: '刷新', exact: true }).click()
  await expect(metrics).toContainText('统计暂不可用')
  await expect(metrics).not.toContainText('150 ms')
})

test('缓存零命中与缺失分开，文案不假定模型价格也不把折扣说成缓存节省', async ({ page }) => {
  await prepare(page)
  await page.goto('/portal?view=tokens')
  const panel = page.getByRole('tabpanel', { name: 'Token 构成', exact: true })
  await expect(panel).toContainText('缓存命中率 0.0%')
  await expect(panel).toContainText('1× 表示与普通输入同价')
  await expect(panel).toContainText('读取上报 2 / 2 次')
  await expect(page.getByText('计费优惠', { exact: true })).toBeVisible()
  const partial = { ...report(), data: [{ ...usageRow, cache_read_known_requests: 1, cache_write_tokens: null }], total: { ...report().total, cache_read_known_requests: 1, cache_hit_bp: null, cache_write_tokens: null } }
  await page.route('**/api/me/stats/breakdown?*', (route) => route.fulfill({ json: partial }))
  await page.getByRole('button', { name: '刷新', exact: true }).click()
  await expect(panel).toContainText('缓存命中率 —')
  await expect(panel).toContainText('缓存数据未完整上报')
  await expect(panel).not.toContainText('缓存命中率 0.0%')
  await expect(panel).not.toContainText('通常 0.1')
})
