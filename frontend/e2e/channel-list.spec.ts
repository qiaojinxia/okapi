import { expect, test } from '@playwright/test'
import type { Page } from '@playwright/test'
import { fileURLToPath } from 'node:url'

// 渠道列表的版面与行内交互：状态分段筛选、「···」行菜单、行内五块信息（渠道 / 状态 / 用量 / 额度 / 调度操作）。
// 接口桩，不碰数据库；固定"现在"让「今天 HH:mm」「x 分钟后」这类文案可重复。

type Json = Record<string, unknown>

const NOW = new Date('2026-10-23T21:00:00')
const nowSec = Math.floor(NOW.getTime() / 1000)
const unix = (iso: string) => Math.floor(new Date(iso).getTime() / 1000)

const key = (over: Json = {}) => ({
  id: 11, status: 1, failed_count: 0, cooldown_until: null, last_error: null, weight: 1,
  max_concurrency: null, credential_kind: 1, oauth_refreshable: true, ...over,
})

const base = {
  priority: 0, settings: {}, pools: ['default'], pool_members: [], cost_milli: 1000,
  egress: { mode: 'inherit' }, last_test: null, last_balance: null,
}

const ALICE = {
  ...base, id: 1, name: 'alice.dev@gmail.com', provider: 'anthropic_max', api_base: 'https://api.anthropic.com',
  status: 1, models: ['claude-sonnet-4-5', 'claude-haiku-4-5'],
  keys: [key({ id: 11, credential_expires_at: unix('2026-10-24T01:50:00'), account_label: 'alice.dev@gmail.com', account_plan: 'pro' })],
  egress: { mode: 'proxy', proxy_id: 1 },
  last_test: { ok: true, latency_ms: 1104, at: '2026-10-16T22:06:00' },
}
const BOB = {
  ...base, id: 2, name: 'bob-team', provider: 'anthropic_max', api_base: 'https://api.anthropic.com',
  status: 1, models: ['claude-sonnet-4-5'],
  keys: [key({ id: 21, credential_expires_at: unix('2026-10-23T23:21:00'), account_label: 'bob@hot-shot.com', account_plan: 'max_5x' })],
  last_test: { ok: true, latency_ms: 1656, at: '2026-10-23T20:24:00' },
}
const CAROL = {
  ...base, id: 3, name: 'carol@outlook.com', provider: 'codex', api_base: 'https://chatgpt.com/backend-api/codex',
  status: 1, priority: 2, models: ['gpt-5.1-codex', 'gpt-5.1'],
  keys: [
    key({ id: 31, credential_expires_at: unix('2026-10-23T08:00:00'), account_label: 'carol@outlook.com', account_plan: 'plus' }),
    key({ id: 32, status: 2, cooldown_until: '2026-10-23T21:20:00', credential_expires_at: unix('2026-10-25T08:00:00'), account_plan: 'plus' }),
  ],
  last_test: { ok: false, latency_ms: 0, error_code: 'upstream_timeout', at: '2026-10-23T19:40:00' },
}
const OPENAI = {
  ...base, id: 4, name: 'openai-main', provider: 'openai', api_base: 'https://api.openai.com/v1',
  status: 1, priority: 5, models: ['gpt-5', 'gpt-5-mini', 'o4-mini'],
  keys: [key({ id: 41, credential_kind: 0 }), key({ id: 42, credential_kind: 0 })],
  pools: [], egress: { mode: 'direct' },
  last_test: { ok: true, latency_ms: 640, at: '2026-10-23T20:41:00' },
  last_balance: { probe: 'openai', currency: 'USD', balance_micro: 12_340_000, at: '2026-10-23T20:45:00' },
}
const SPARE = {
  ...base, id: 5, name: 'spare-compat', provider: 'openai_compat', api_base: 'https://relay.example.com/v1',
  status: 2, models: ['gpt-4o-mini'], keys: [key({ id: 51, credential_kind: 0 })],
}
const CHANNELS: Json[] = [ALICE, BOB, CAROL, OPENAI, SPARE]

const PROVIDERS = {
  data: [
    { id: 'openai', account: { quota: false, refresh: false, subscription: null } },
    { id: 'openai_compat', account: { quota: false, refresh: false, subscription: null } },
    { id: 'anthropic_max', account: { quota: true, refresh: true, subscription: { quota_scope: 'session', window_secs: 18000, quota_windows: [18000, 604800] } } },
    { id: 'codex', account: { quota: true, refresh: true, subscription: { quota_scope: 'total', window_secs: null, quota_windows: [18000, 604800] } } },
  ],
}

const quota = (windows: Json[]) => ({ timezone: 'UTC', quotas: [{ key_id: 1, quota: { observed_at: nowSec, allowed: true, windows } }] })
const QUOTAS: Record<string, Json> = {
  '1': quota([
    { name: '5h', used_percent: 0, resets_at: null, window_secs: 18000 },
    { name: '7d', used_percent: 0, resets_at: nowSec + (5 * 24 + 22) * 3600, window_secs: 604800 },
  ]),
  '2': quota([
    { name: '5h', used_percent: 55, resets_at: nowSec + 15 * 60, window_secs: 18000 },
    { name: '7d', used_percent: 18, resets_at: nowSec + (5 * 24 + 10) * 3600, window_secs: 604800 },
  ]),
  '3': quota([
    { name: '5h', used_percent: 92, resets_at: nowSec + 50 * 60, window_secs: 18000 },
    { name: '7d', used_percent: 74, resets_at: nowSec + (2 * 24 + 3) * 3600, window_secs: 604800 },
  ]),
}

const HEALTH = {
  data: [
    { channel_id: 1, name: 'a', provider: 'anthropic_max', requests: 1, errors: 0, error_rate_bp: 0, ttft_p50_ms: 800, ttft_p95_ms: 900, ttft_p99_ms: 900, failovers: 0, sticky_rate_bp: 0, tokens_per_1k_sec: 0, amount_micro: 0 },
    { channel_id: 2, name: 'b', provider: 'anthropic_max', requests: 536, errors: 0, error_rate_bp: 0, ttft_p50_ms: 900, ttft_p95_ms: 1800, ttft_p99_ms: 2400, failovers: 0, sticky_rate_bp: 0, tokens_per_1k_sec: 0, amount_micro: 0 },
    { channel_id: 3, name: 'c', provider: 'codex', requests: 80, errors: 12, error_rate_bp: 1500, ttft_p50_ms: 900, ttft_p95_ms: 3000, ttft_p99_ms: 4000, failovers: 5, sticky_rate_bp: 0, tokens_per_1k_sec: 0, amount_micro: 0 },
  ],
}

const USAGE = {
  data: [
    { channel_id: 1, requests: 2085, prompt_tokens: 600_000_000, cached_tokens: 400_000_000, completion_tokens: 40_000_000, amount_micro: 314_680_000, upstream_cost_micro: 200_000_000 },
    { channel_id: 2, requests: 4391, prompt_tokens: 1_500_000_000, cached_tokens: 900_000_000, completion_tokens: 130_000_000, amount_micro: 737_870_000, upstream_cost_micro: 500_000_000 },
  ],
}

async function prepare(page: Page, { onList, path = '/admin/channels' }: { onList?: (url: URL) => void; path?: string } = {}) {
  await page.addInitScript(() => {
    localStorage.setItem('okapi.key', 'channel-list-fixture')
    localStorage.setItem('okapi.lang', 'zh-CN')
  })
  await page.clock.setFixedTime(NOW)
  await page.route('**/*', async (route) => {
    const request = route.request()
    const url = new URL(request.url())
    const path = url.pathname
    if (request.isNavigationRequest()) {
      return route.fulfill({ path: fileURLToPath(new URL('../dist/index.html', import.meta.url)), contentType: 'text/html' })
    }
    if (!/^\/(api|admin|auth)\//.test(path)) return route.continue()
    expect(request.method(), `未被桩接住的写请求：${path}`).toBe('GET')
    const quotaId = /^\/admin\/channels\/(\d+)\/usage$/.exec(path)?.[1]
    let json: Json
    if (path === '/admin/channels') {
      onList?.(url)
      const q = url.searchParams.get('q')?.toLowerCase() ?? ''
      const provider = url.searchParams.get('provider')
      const status = url.searchParams.get('status')
      const hit = CHANNELS.filter(
        (c) =>
          (q === '' || String(c.name).toLowerCase().includes(q)) &&
          (provider === null || c.provider === provider) &&
          (status === null || String(c.status) === status),
      )
      const offset = Number(url.searchParams.get('offset') ?? 0)
      const limit = url.searchParams.has('limit') ? Number(url.searchParams.get('limit')) : hit.length
      json = { data: hit.slice(offset, offset + limit), total: hit.length, enabled: hit.filter((c) => c.status === 1).length }
    } else {
      json =
        path === '/api/me' ? { user_id: 1, key_id: 1, group: 'default', balance_micro: 10_000_000, role: 100, permissions: ['*'] }
        : path === '/api/notice' ? { notice: null }
        : path === '/admin/channels/providers' ? PROVIDERS
        : path === '/admin/stats/channels' ? HEALTH
        : path === '/admin/stats/channel-usage' ? USAGE
        : quotaId !== undefined ? (QUOTAS[quotaId] ?? { timezone: 'UTC', quotas: [] })
        : path === '/admin/proxies' ? { data: [{ id: 1, name: '216.167.15.231', status: 1 }], total: 1 }
        : path.startsWith('/admin/settings/') ? { value: null }
        : { data: [], total: 0 }
    }
    return route.fulfill({ json })
  })
  await page.goto(path)
  await expect(page.locator('#main-content').getByRole('heading', { name: '渠道' })).toBeVisible()
}

const rowOf =(page: Page, name: string) => page.getByRole('row').filter({ hasText: name })
// 订阅账号邮箱在界面上遮住中间部分：渠道名、账号行、开关与勾选框的名字都是遮过的
const ALICE_MASKED = 'al***ev@gmail.com'
const BOB_MASKED = 'b***@hot-shot.com'
const CAROL_MASKED = 'c***l@outlook.com'

test('视觉评审：渠道页当前设计截图', async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  await prepare(page)
  await expect(page.getByRole('progressbar').first()).toBeVisible()
  await page.screenshot({ path: 'test-results/channel-after.png', fullPage: true })
  await page.evaluate(() => document.documentElement.classList.add('dark'))
  await page.waitForTimeout(300)
  await page.screenshot({ path: 'test-results/channel-after-dark.png', fullPage: true })
  await page.evaluate(() => document.documentElement.classList.remove('dark'))
  await page.waitForTimeout(300)
  await page.setViewportSize({ width: 1280, height: 900 })
  await page.screenshot({ path: 'test-results/channel-after-1280.png', fullPage: true })
  await page.setViewportSize({ width: 390, height: 844 })
  await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true)
  await page.screenshot({ path: 'test-results/channel-after-mobile.png', fullPage: true })
})

test('视觉评审：渠道页英文截图', async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  await prepare(page)
  await page.addInitScript(() => localStorage.setItem('okapi.lang', 'en'))
  await page.reload()
  await expect(page.locator('#main-content').getByRole('heading', { name: 'Channels' })).toBeVisible()
  await expect(page.getByRole('progressbar').first()).toBeVisible()
  await page.screenshot({ path: 'test-results/channel-after-en.png', fullPage: true })
})

test('视觉评审：渠道编辑抽屉截图', async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  await prepare(page)
  await rowOf(page, 'openai-main').getByRole('button', { name: '编辑', exact: true }).click()
  const drawer = page.getByRole('dialog')
  await expect(drawer.getByRole('heading', { name: /编辑渠道/ })).toBeVisible()
  await page.waitForTimeout(350)
  await page.screenshot({ path: 'test-results/channel-drawer-conn.png', fullPage: true })
  for (const [tab, name] of [['模型与调度池', 'models'], ['调度', 'sched'], ['请求与计费行为', 'behavior']] as const) {
    await drawer.getByRole('tab', { name: tab, exact: true }).click()
    await page.waitForTimeout(200)
    await page.screenshot({ path: `test-results/channel-drawer-${name}.png`, fullPage: true })
  }
  await drawer.getByRole('tab', { name: '接入信息', exact: true }).click()
  await page.evaluate(() => document.documentElement.classList.add('dark'))
  await page.waitForTimeout(300)
  await page.screenshot({ path: 'test-results/channel-drawer-dark.png', fullPage: true })
})

test('渠道列表：页头统计是朴素文字，新建按钮沿用全局图标与排版', async ({ page }) => {
  await prepare(page)
  const header = page.locator('[data-slot="page-header"]')
  await expect(header).toContainText('共 5 条 · 4 启用')
  await expect(header.locator('[class*="rounded-full"]')).toHaveCount(0)
  await expect(header.locator('h1')).toHaveCSS('font-size', '20px')
  const create = header.getByRole('button', { name: '新建渠道', exact: true })
  await expect(create).toBeVisible()
  await expect(create.locator('svg')).toHaveAttribute('aria-hidden', 'true')
  await expect(header.locator('svg')).toHaveCount(1)
  await expect(header.getByRole('button', { name: '测试全部启用渠道', exact: true })).toBeVisible()
  await expect(header.getByRole('button', { name: '路由诊断', exact: true })).toBeVisible()
  // 筛选直接放在页面底色上，没有再包一层工具栏卡片
  const toolbar = page.locator('[data-slot="toolbar"]')
  await expect(toolbar).toBeVisible()
  await expect(toolbar).not.toHaveClass(/border/)
  // 搜索框没有放大镜，也没有单独的「搜索」按钮——回车提交
  await expect(toolbar.locator('svg')).toHaveCount(1) // 只剩协议下拉的箭头
  await expect(toolbar.getByRole('button', { name: '搜索', exact: true })).toHaveCount(0)
})

test('渠道列表：状态分段筛选把 status 带到接口与地址，与搜索 / 协议组合，清空筛选一起复位', async ({ page }) => {
  const hits: string[] = []
  await prepare(page, { onList: (url) => hits.push(url.search) })
  const rows = page.locator('tbody tr')
  const status = page.getByRole('group', { name: '状态', exact: true })
  await expect(rows).toHaveCount(5)
  expect(hits.at(-1)).toBe('?limit=10&offset=0')
  await expect(status.getByRole('button', { name: '全部', exact: true })).toHaveAttribute('aria-pressed', 'true')

  await status.getByRole('button', { name: '已停用', exact: true }).click()
  await expect(page).toHaveURL(/status=disabled/)
  await expect(rows).toHaveCount(1)
  await expect(rows.first()).toContainText('spare-compat')
  expect(hits.at(-1)).toBe('?limit=10&offset=0&status=2')
  await expect(status.getByRole('button', { name: '已停用', exact: true })).toHaveAttribute('aria-pressed', 'true')
  // 页头统计是筛选集内的计数：停用集里没有启用的
  await expect(page.locator('[data-slot="page-header"]')).toContainText('共 1 条 · 0 启用')

  await status.getByRole('button', { name: '已启用', exact: true }).click()
  await expect(page).toHaveURL(/status=enabled/)
  await expect(rows).toHaveCount(4)
  expect(hits.at(-1)).toBe('?limit=10&offset=0&status=1')
  await expect(page.locator('[data-slot="page-header"]')).toContainText('共 4 条 · 4 启用')

  // 搜索 + 协议 + 状态：参数按 q、provider、status 的顺序一起带上
  await page.locator('#ch-provider').selectOption('anthropic_max')
  await expect(rows).toHaveCount(2)
  await page.getByPlaceholder('按名称或地址搜索').fill('bob')
  await page.getByPlaceholder('按名称或地址搜索').press('Enter')
  await expect(rows).toHaveCount(1)
  expect(hits.at(-1)).toBe('?limit=10&offset=0&q=bob&provider=anthropic_max&status=1')

  // 状态筛到没有结果时给出「清空筛选」，点了一起复位
  await status.getByRole('button', { name: '已停用', exact: true }).click()
  await expect(page.getByText('没有匹配的结果')).toBeVisible()
  await page.getByRole('button', { name: '清空筛选' }).first().click()
  await expect(page).not.toHaveURL(/status=/)
  await expect(page).not.toHaveURL(/provider=/)
  await expect(page).not.toHaveURL(/q=/)
  await expect(rows).toHaveCount(5)
  await expect(status.getByRole('button', { name: '全部', exact: true })).toHaveAttribute('aria-pressed', 'true')
  expect(hits.at(-1)).toBe('?limit=10&offset=0')
})

test('渠道列表：地址里手敲 status=enabled 直接生效，数字和别的非法值都忽略', async ({ page }) => {
  const hits: string[] = []
  await prepare(page, { path: '/admin/channels?status=enabled', onList: (url) => hits.push(url.search) })
  await expect(page.locator('tbody tr')).toHaveCount(4)
  await expect(page.getByRole('group', { name: '状态', exact: true }).getByRole('button', { name: '已启用', exact: true })).toHaveAttribute('aria-pressed', 'true')
  expect(hits.at(-1)).toBe('?limit=10&offset=0&status=1')
  await page.goto('/admin/channels?status=9')
  await expect(page.locator('tbody tr')).toHaveCount(5)
  expect(hits.at(-1)).toBe('?limit=10&offset=0')
})

test('渠道列表：双层结构化渠道卡片行——上层身份路由与实时控制，下层额度 / 健康 / 近 30 天遥测', async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  await prepare(page)
  const alice = rowOf(page, ALICE_MASKED)
  await expect(alice).toBeVisible()
  await expect(page.locator('[data-slot="channel-card"]')).toHaveCount(5)

  // 上层左侧：名称（粗体）+ #ID + 平台档位胶囊 + 出口 + 常驻路由信息
  await expect(alice.locator('[data-slot="channel-name"]')).toHaveText(ALICE_MASKED)
  await expect(alice.locator('[data-slot="channel-name"]')).toHaveCSS('font-weight', '600')
  await expect(alice.getByText('#1', { exact: true })).toBeVisible()
  await expect(alice.locator('[data-slot="provider-chip"]')).toHaveText('Claude Pro')
  // 订阅渠道都打官方地址，不逐行重复；邮箱就是渠道名时也不重复账号
  await expect(alice).not.toContainText('api.anthropic.com')
  await expect(alice.locator('[data-slot="channel-account"]')).toHaveCount(0)
  await expect(alice.locator('[data-slot="channel-host"]')).toHaveCount(0)
  const meta = alice.locator('[data-slot="channel-meta"]')
  await expect(meta.locator('[data-slot="channel-egress"]').getByTitle('代理 216.167.15.231', { exact: true })).toHaveText('216.167.15.231')
  const schedule = alice.locator('[data-slot="channel-schedule"]')
  await expect(schedule).toContainText('default')
  await expect(schedule).toContainText('2 个模型')
  await expect(schedule).toContainText('优先级 0')

  // 上层右侧：实时状态胶囊 + 最近测试 + 调度开关 + 操作按钮组
  const state = alice.locator('[data-slot="channel-status"]')
  await expect(state).toContainText('正常')
  await expect(state.locator('[data-slot="last-probe"]')).toContainText('1104 ms')
  await expect(state.locator('[data-slot="last-probe"]')).toContainText('10-16 22:06')
  await expect(alice.locator('[data-slot="key-state-detail"]')).toHaveCount(0)
  await expect(alice.getByRole('switch', { name: `参与调度：${ALICE_MASKED}` })).toBeChecked()
  // 凭证到期属于编辑详情，不占列表
  await expect(alice).not.toContainText('凭证')
  await expect(alice).not.toContainText('10-24 01:50 到期')

  // 下层区域一（额度）：每个窗口独占一行竖向排列
  const fiveHour = alice.getByRole('progressbar', { name: '5小时额度用量' })
  const sevenDay = alice.getByRole('progressbar', { name: '7天额度用量' })
  await expect(fiveHour).toHaveAttribute('aria-valuenow', '0')
  await expect(sevenDay).toHaveAttribute('aria-valuenow', '0')
  await expect(alice.locator('[data-slot="quota-meter"]')).toHaveCount(2)
  await expect(alice.locator('[data-slot="quota-meter"]').first()).toContainText('5h')
  await expect(alice.locator('[data-slot="quota-meter"]').nth(1)).toContainText('7d')
  await expect(alice.locator('[data-slot="quota-reset"]').nth(1)).toHaveText('5天22小时')

  // 下层区域二（24h 健康与 TTFT）
  const health = alice.locator('[data-slot="channel-health"]')
  await expect(health.locator('[data-slot="health-24h"]')).toHaveText('近 24h 0.0% 错误 · 1 次')
  await expect(health.locator('[data-slot="health-ttft"]')).toContainText('TTFT p50 800 ms · p95 900 ms')

  // 下层区域三（近 30 天）
  const usage = alice.locator('[data-slot="channel-usage"]')
  const spend = alice.locator('[data-slot="channel-spend"]')
  await expect(spend).toHaveText('$314.68')
  await expect(spend).toHaveCSS('font-weight', '600')
  await expect(usage).toContainText('2,085 次 · 6.4亿 tokens')
  await expect(alice).not.toContainText('US$314.68')

  // 操作格：测试 / 编辑 / 更多操作三个图标按钮
  const actions = alice.getByRole('group', { name: '操作', exact: true })
  await expect(actions).toHaveAttribute('data-slot', 'action-group')
  for (const name of ['测试', '编辑', '更多操作']) {
    const button = actions.getByRole('button', { name, exact: true })
    await expect(button).toBeVisible()
    await expect(button).toHaveText('')
    await expect(button.locator('svg')).toHaveCount(1)
  }
  await expect(alice.getByRole('button', { name: '复制', exact: true })).toHaveCount(0)
  await expect(alice.getByRole('button', { name: '删除', exact: true })).toHaveCount(0)
  await expect(alice.getByRole('button', { name: '测试输出', exact: true })).toHaveCount(0)
})

test('渠道列表：额度条只用红黄绿三种颜色；账号与渠道名不同时在第二行补出账号', async ({ page }) => {
  await prepare(page)
  const bob = rowOf(page, 'bob-team')
  await expect(bob).toContainText('Claude Max 5x')
  // 邮箱不是渠道名：第二行胶囊后面补出账号（遮过的），「账号」二字只给读屏
  await expect(bob.locator('[data-slot="channel-account"]')).toHaveText(BOB_MASKED)
  await expect(bob.locator('[data-slot="channel-meta"]')).toContainText('账号')
  await expect(bob).not.toContainText('bob@hot-shot.com')
  const bobFive = bob.getByRole('progressbar', { name: '5小时额度用量' })
  const bobSeven = bob.getByRole('progressbar', { name: '7天额度用量' })
  await expect(bobFive).toHaveAttribute('aria-valuenow', '55')
  await expect(bobSeven).toHaveAttribute('aria-valuenow', '18')
  // 5 小时和 7 天都不到 70%：同一种绿，不再按窗口各配一种颜色
  await expect(bobFive.locator('div')).toHaveClass(/bg-success/)
  await expect(bobSeven.locator('div')).toHaveClass(/bg-success/)
  await expect(bob).toContainText('15分钟')

  const carol = rowOf(page, CAROL_MASKED)
  await expect(carol.getByRole('progressbar', { name: '5小时额度用量' }).locator('div')).toHaveClass(/bg-destructive/)
  await expect(carol.getByRole('progressbar', { name: '7天额度用量' }).locator('div')).toHaveClass(/bg-warning/)
})

test('渠道列表：额度仪表竖向分层展示，进度条跨卡片严格对齐且宽度充裕，重置倒计时在右侧', async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  await prepare(page)
  await expect(page.getByRole('progressbar')).toHaveCount(6)
  const boxes = []
  for (const bar of await page.getByRole('progressbar').all()) boxes.push((await bar.boundingBox())!)
  // 跨所有订阅卡片的每一条进度条：同一竖列左缘对齐、完全等宽，且拥有充裕的读数宽度
  for (const box of boxes) {
    expect(Math.abs(box.x - boxes[0].x)).toBeLessThanOrEqual(1)
    expect(Math.abs(box.width - boxes[0].width)).toBeLessThanOrEqual(1)
    expect(box.width).toBeGreaterThanOrEqual(180)
  }
  // 同一张卡片内：5h 在上、7d 在下，不再挤在同一行
  expect(boxes[1].y).toBeGreaterThan(boxes[0].y + boxes[0].height)
  expect(boxes[3].y).toBeGreaterThan(boxes[2].y + boxes[2].height)

  // 重置说明位于对应进度条的右侧同一行
  const bob = rowOf(page, 'bob-team')
  const resets = bob.locator('[data-slot="quota-reset"]')
  await expect(resets).toHaveText(['15分钟', '5天10小时'])
  const bars = [(await bob.getByRole('progressbar').nth(0).boundingBox())!, (await bob.getByRole('progressbar').nth(1).boundingBox())!]
  for (const index of [0, 1]) {
    const reset = (await resets.nth(index).boundingBox())!
    expect(reset.x).toBeGreaterThan(bars[index].x + bars[index].width)
    expect(Math.abs(reset.y + reset.height / 2 - (bars[index].y + bars[index].height / 2))).toBeLessThanOrEqual(4)
  }
})

test('渠道列表：部分 key 不可用时展开告警条并提示恢复时间；停用与未入池各有清晰标识', async ({ page }) => {
  await prepare(page)
  const carol = rowOf(page, CAROL_MASKED)
  const status = carol.locator('[data-slot="channel-status"]')
  await expect(status).toContainText('1/2 可用')
  await expect(status.locator('[data-slot="last-probe"]')).toContainText('upstream_timeout')
  // 异常 key 诊断条独立展开，不挤进首行状态胶囊
  const detail = carol.locator('[data-slot="key-state-detail"]')
  await expect(detail).toContainText('冷却 1')
  await expect(detail).toContainText('约 20 分钟后恢复')
  await expect(detail).not.toContainText('upstream_timeout')
  await expect(status).not.toContainText('冷却 1')
  const health = carol.locator('[data-slot="health-24h"]')
  await expect(health).toContainText('15.0% 错误 · 80 次')
  await expect(carol.locator('[data-slot="health-ttft"]')).toContainText('渠道切换 5')
  // 自上而下三层：上层状态 → 诊断条 → 下层健康遥测
  const ys = []
  for (const part of [status, detail, health]) ys.push((await part.boundingBox())!.y)
  expect(ys[0]).toBeLessThan(ys[1])
  expect(ys[1]).toBeLessThan(ys[2])
  await expect(carol).not.toContainText('已过期')
  // 有告警条的卡片高于同类型的健康卡片
  expect((await carol.boundingBox())!.height).toBeGreaterThan((await rowOf(page, 'bob-team').boundingBox())!.height + 12)

  const openai = rowOf(page, 'openai-main')
  await expect(openai.locator('[data-slot="channel-status"]')).toContainText('未入池')
  await expect(openai.locator('[data-slot="provider-chip"]')).toHaveText('OpenAI API')
  await expect(openai.locator('[data-slot="channel-host"]')).toHaveText('api.openai.com')
  await expect(openai.locator('[data-slot="channel-host"]')).toHaveAttribute('title', 'https://api.openai.com/v1')
  await expect(openai.locator('[data-slot="channel-egress"]')).toContainText('直连')
  const balance = openai.locator('[data-slot="channel-balance"]')
  await expect(balance).toContainText('$12.34')
  await expect(balance).toContainText('余额')
  await expect(balance).toContainText('20:45 查询')
  const usage = openai.locator('[data-slot="channel-usage"]')
  await expect(usage).toContainText('—')
  await expect(usage).toContainText('近 30 天暂无计费调用')
  await expect(usage).toHaveAttribute('title', '近 30 天暂无计费调用')

  const spare = rowOf(page, 'spare-compat')
  await expect(spare.locator('[data-slot="channel-status"]')).toContainText('停用')
  await expect(spare.getByRole('switch', { name: '参与调度：spare-compat' })).not.toBeChecked()
  await expect(spare).toContainText('未测过')
  await expect(spare).toContainText('无请求')
  await expect(spare.locator('[data-slot="channel-name"]')).toHaveClass(/text-muted-foreground/)
  await expect(rowOf(page, 'openai-main').locator('[data-slot="channel-name"]')).not.toHaveClass(/text-muted-foreground/)
  await expect(spare.getByRole('progressbar')).toHaveCount(0)
})

test('渠道列表：供应商控制台入口平时收起，悬停整行或键盘聚焦才现身', async ({ page }) => {
  await prepare(page)
  const openai = rowOf(page, 'openai-main')
  const link = openai.getByRole('link', { name: '打开供应商控制台' })
  await expect(link).toHaveAttribute('href', 'https://platform.openai.com/usage')
  await expect(link).toHaveCSS('opacity', '0')
  await openai.hover()
  await expect(link).toHaveCSS('opacity', '1')
  await page.mouse.move(0, 0)
  await expect(link).toHaveCSS('opacity', '0')
  await link.focus()
  await expect(link).toHaveCSS('opacity', '1')
})

test('渠道列表：「···」菜单收纳试聊 / 余额 / 额度上限 / 复制 / 删除，键盘可开可关', async ({ page }) => {
  await prepare(page)
  const openai = rowOf(page, 'openai-main')
  const more = openai.getByRole('button', { name: '更多操作', exact: true })
  await expect(more).toHaveAttribute('aria-haspopup', 'menu')
  await expect(more).toHaveAttribute('aria-expanded', 'false')
  await more.click()
  const menu = page.getByRole('menu')
  await expect(menu).toBeVisible()
  await expect(more).toHaveAttribute('aria-expanded', 'true')
  // openai 有余额接口：七项都在；删除排在分隔线之后
  await expect(menu.getByRole('menuitem')).toHaveText(['测试输出', '模型验真', '查上游余额', '额度上限', '复制', '作为模板新建', '删除'])
  // 打开后焦点落在第一项
  await expect(menu.getByRole('menuitem', { name: '测试输出', exact: true })).toBeFocused()
  await page.keyboard.press('ArrowDown')
  await expect(menu.getByRole('menuitem', { name: '模型验真', exact: true })).toBeFocused()
  await page.keyboard.press('Escape')
  await expect(menu).toHaveCount(0)
  await expect(more).toBeFocused()

  // 键盘：触发器上按 ↑ 直接从最后一项开始
  await page.keyboard.press('ArrowUp')
  await expect(page.getByRole('menuitem', { name: '删除', exact: true })).toBeFocused()
  await page.keyboard.press('Escape')

  // 没有余额接口的协议不列「查上游余额」
  const alice = rowOf(page, ALICE_MASKED)
  await alice.getByRole('button', { name: '更多操作', exact: true }).click()
  await expect(page.getByRole('menu').getByRole('menuitem')).toHaveText(['测试输出', '模型验真', '额度上限', '复制', '作为模板新建', '删除'])
  // 点菜单外收起
  await page.locator('#main-content').getByRole('heading', { name: '渠道' }).click()
  await expect(page.getByRole('menu')).toHaveCount(0)
})

test('渠道列表：菜单项走原有动作——试聊开抽屉、复制带 -copy、删除要手输名称', async ({ page }) => {
  const calls: { path: string; method: string; body: Json | null }[] = []
  await prepare(page)
  await page.route('**/admin/channels/*/duplicate', async (route) => {
    const r = route.request()
    calls.push({ path: new URL(r.url()).pathname, method: r.method(), body: (r.postDataJSON() ?? null) as Json | null })
    await route.fulfill({ json: { ok: true } })
  })
  await page.route(/\/admin\/channels\/\d+$/, async (route) => {
    if (route.request().method() !== 'DELETE') return route.fallback()
    const r = route.request()
    calls.push({ path: new URL(r.url()).pathname, method: r.method(), body: null })
    await route.fulfill({ json: { ok: true } })
  })
  const openai = rowOf(page, 'openai-main')
  const open = async (name: string) => {
    await openai.getByRole('button', { name: '更多操作', exact: true }).click()
    await page.getByRole('menuitem', { name, exact: true }).click()
  }

  await open('复制')
  await expect.poll(() => calls.length).toBe(1)
  expect(calls[0]).toEqual({ path: '/admin/channels/4/duplicate', method: 'POST', body: { name: 'openai-main-copy' } })

  await open('测试输出')
  await expect(page.getByRole('dialog')).toBeVisible()
  await page.keyboard.press('Escape')
  await expect(page.getByRole('dialog')).toHaveCount(0)

  await open('删除')
  const confirm = page.getByRole('alertdialog')
  await expect(confirm.getByRole('button', { name: '删除', exact: true })).toBeDisabled()
  await confirm.locator('#confirm-text').fill('openai-main')
  await confirm.getByRole('button', { name: '删除', exact: true }).click()
  await expect.poll(() => calls.some((c) => c.method === 'DELETE' && c.path === '/admin/channels/4')).toBe(true)
})

test('渠道列表：菜单浮在表格之上不被裁掉；末行放不下就向上翻；触发器一动就收起', async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 420 })
  await prepare(page)
  // 等异步内容就位，避免菜单打开后行高变化把触发器移走。
  await expect(page.getByRole('progressbar')).toHaveCount(6)
  await expect(page.locator('[data-slot="channel-spend"]').first()).toHaveText('$314.68')
  await expect(rowOf(page, ALICE_MASKED)).toContainText('216.167.15.231')
  const viewport = page.viewportSize()!
  const last = rowOf(page, 'spare-compat').getByRole('button', { name: '更多操作', exact: true })
  // 滚到表格底：末行贴着窗口下沿，下方放不下菜单。直接 el.click()，
  // 免得 Playwright 为了点击又把触发器滚回视野中央
  await page.locator('[data-slot="table-viewport"]').evaluate((el) => {
    el.scrollTop = el.scrollHeight
  })
  await last.evaluate((el) => (el as HTMLElement).click())
  const menu = page.getByRole('menu')
  await expect(menu).toBeVisible()
  const menuBox = (await menu.boundingBox())!
  const triggerBox = (await last.boundingBox())!
  // 前提：触发器下方确实放不下整个菜单，否则这条测的不是「翻到上方」
  expect(viewport.height - (triggerBox.y + triggerBox.height)).toBeLessThan(menuBox.height + 14)
  // 完整落在窗口里，并且没有被表格的滚动容器裁掉
  expect(menuBox.y).toBeGreaterThanOrEqual(0)
  expect(menuBox.y + menuBox.height).toBeLessThanOrEqual(viewport.height)
  expect(menuBox.x + menuBox.width).toBeLessThanOrEqual(viewport.width)
  // 翻到触发器上方
  expect(menuBox.y + menuBox.height).toBeLessThanOrEqual(triggerBox.y)
  // 菜单项都能点到（没被别的元素盖住）
  await menu.getByRole('menuitem', { name: '复制', exact: true }).hover()

  // 表格滚动把触发器带走时，菜单跟着收起，不悬在半空
  await page.locator('[data-slot="table-viewport"]').evaluate((el) => {
    el.scrollTop = el.scrollTop === 0 ? 60 : 0
  })
  await expect(menu).toHaveCount(0)
})

test('渠道列表：窗口缩放时「···」菜单收起；窄屏卡片自适应重排无横向滚动', async ({ page }) => {
  await prepare(page)
  await rowOf(page, ALICE_MASKED).getByRole('button', { name: '更多操作', exact: true }).click()
  await expect(page.getByRole('menu')).toBeVisible()
  await page.setViewportSize({ width: 1100, height: 800 })
  await expect(page.getByRole('menu')).toHaveCount(0)

  for (const width of [800, 390]) {
    await page.setViewportSize({ width, height: 844 })
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true)
  }
})

test('渠道列表：1280 宽下卡片不出现横向滚动，路由信息完整可见，内容不越出卡片右缘', async ({ page }) => {
  await prepare(page)
  const viewport = page.locator('[data-slot="table-viewport"]')
  await expect(viewport).toBeVisible()
  expect(await viewport.evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true)
  const alice = rowOf(page, ALICE_MASKED)
  await expect(alice.getByText('#1', { exact: true })).toBeVisible()
  await expect(alice.locator('[data-slot="last-probe"]')).toContainText('1104 ms')
  await expect(alice.locator('[data-slot="channel-schedule"]')).toContainText('优先级 0')
  await expect(alice.locator('[data-slot="channel-schedule"]')).toContainText('2 个模型')
  await expect(alice.getByRole('switch')).toBeVisible()
  await expect(alice.getByRole('group', { name: '操作', exact: true })).toBeVisible()
  for (const name of [ALICE_MASKED, 'bob-team', CAROL_MASKED, 'openai-main']) {
    const overflow = await rowOf(page, name).locator('[data-slot="channel-card"]').evaluate((card) => {
      const box = card.getBoundingClientRect()
      return Math.max(0, ...[...card.querySelectorAll('*')].filter((node) => !node.classList.contains('sr-only') && node.getClientRects().length > 0)
        .map((node) => { const inner = node.getBoundingClientRect(); return inner.width === 0 ? 0 : inner.right - box.right }))
    })
    expect(overflow).toBeLessThanOrEqual(1)
  }
})

test('渠道列表：宽屏（1608）下额度条充裕舒展，近 30 天金额右缘逐卡对齐', async ({ page }) => {
  await page.setViewportSize({ width: 1608, height: 900 })
  await prepare(page)
  const viewport = page.locator('[data-slot="table-viewport"]')
  expect(await viewport.evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true)
  const alice = rowOf(page, ALICE_MASKED)
  const bob = rowOf(page, 'bob-team')
  const bar = (await alice.getByRole('progressbar', { name: '5小时额度用量' }).boundingBox())!
  expect(bar.width).toBeGreaterThanOrEqual(220)
  const amount = (await alice.locator('[data-slot="channel-spend"]').boundingBox())!
  const other = (await bob.locator('[data-slot="channel-spend"]').boundingBox())!
  expect(Math.abs(amount.x + amount.width - (other.x + other.width))).toBeLessThanOrEqual(1)
  const requests = (await alice.getByText(/2,085 次/).boundingBox())!
  expect(requests.y).toBeGreaterThan(amount.y + amount.height - 2)
  expect(Math.abs(requests.x + requests.width - (amount.x + amount.width))).toBeLessThanOrEqual(1)
})

test('渠道列表：深色主题下状态点与额度条仍用语义色', async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem('okapi.theme', 'dark'))
  await prepare(page)
  await expect(page.locator('html')).toHaveClass(/dark/)
  const carol = rowOf(page, CAROL_MASKED)
  await expect(carol.locator('[data-slot="key-state-summary"]')).toHaveClass(/text-warning/)
  await expect(carol.getByRole('progressbar', { name: '5小时额度用量' }).locator('div')).toHaveClass(/bg-destructive/)
})

test('渠道列表：额度条只有红黄绿三种颜色表示剩余，边界 70% 转黄、90% 转红', async ({ page }) => {
  await prepare(page)
  // 渠道 1 / 2 的额度改成踩着阈值的几组数
  const windows = (five: number, seven: number) => quota([
    { name: '5h', used_percent: five, resets_at: nowSec + 3600, window_secs: 18000 },
    { name: '7d', used_percent: seven, resets_at: nowSec + 3 * 86400, window_secs: 604800 },
  ])
  await page.route('**/admin/channels/1/usage?*', (route) => route.fulfill({ json: windows(69, 70) }))
  await page.route('**/admin/channels/2/usage?*', (route) => route.fulfill({ json: windows(89, 90) }))
  await page.reload()
  const bar = (row: string, label: string) => rowOf(page, row).getByRole('progressbar', { name: label }).locator('div')
  await expect(bar(ALICE_MASKED, '5小时额度用量')).toHaveClass(/bg-success/)
  await expect(bar(ALICE_MASKED, '7天额度用量')).toHaveClass(/bg-warning/)
  await expect(bar('bob-team', '5小时额度用量')).toHaveClass(/bg-warning/)
  await expect(bar('bob-team', '7天额度用量')).toHaveClass(/bg-destructive/)
  // 整张表里的额度条颜色只会是这三种，不会再出现琥珀 / 主色 / 其他色相
  const classes = await page.getByRole('progressbar').evaluateAll((nodes) => nodes.map((node) => node.firstElementChild?.className ?? ''))
  expect(classes.length).toBeGreaterThanOrEqual(6)
  for (const name of classes) {
    expect(name, name).toMatch(/\bbg-(success|warning|destructive)\b/)
    expect(name, name).not.toMatch(/\bbg-(amber|primary|info|blue|sky|emerald|green|red|yellow|orange|purple)\b/)
  }
})

test('渠道列表：测试 / 编辑 / 更多是图标按钮：悬停是便签提示，测试进行中转圈并禁用，结果提示里的渠道名已遮住', async ({ page }) => {
  await prepare(page)
  let release: () => void = () => undefined
  const gate = new Promise<void>((resolve) => { release = resolve })
  await page.route('**/admin/channels/1/test', async (route) => {
    await gate
    await route.fulfill({ json: { ok: true, latency_ms: 321 } })
  })
  const alice = rowOf(page, ALICE_MASKED)
  const test = alice.getByRole('button', { name: '测试', exact: true })
  const edit = alice.getByRole('button', { name: '编辑', exact: true })
  const more = alice.getByRole('button', { name: '更多操作', exact: true })
  // 三个一样大的图标按钮并排
  const boxes = await Promise.all([test, edit, more].map(async (button) => (await button.boundingBox())!))
  for (const box of boxes) {
    expect(box.width).toBeCloseTo(32, 1)
    expect(box.height).toBeCloseTo(32, 1)
  }
  expect(Math.abs(boxes[0].y - boxes[1].y)).toBeLessThanOrEqual(1)
  expect(Math.abs(boxes[1].y - boxes[2].y)).toBeLessThanOrEqual(1)
  // 悬停：便签样式的提示，字是按钮的名字
  await edit.hover()
  const tip = page.getByRole('tooltip')
  await expect(tip).toHaveText('编辑')
  await expect(tip).toHaveClass(/\bnote\b/)
  await page.mouse.move(0, 0)
  // 点测试：进行中按钮禁用并转圈，别的行不受影响
  await test.click()
  await expect(test).toBeDisabled()
  await expect(test).toHaveAttribute('aria-busy', 'true')
  await expect(test.locator('svg.animate-spin')).toBeVisible()
  await expect(rowOf(page, BOB_MASKED).getByRole('button', { name: '测试', exact: true })).toBeEnabled()
  release()
  // 结果 toast 的标题是渠道名：遮过的，不带完整邮箱
  const toast = page.getByText('321', { exact: false }).first()
  await expect(toast).toBeVisible()
  await expect(page.getByText(ALICE_MASKED).first()).toBeVisible()
  await expect(test).toBeEnabled()
  expect(await page.locator('body').innerText()).not.toContain('alice.dev@gmail.com')
})

test('渠道列表：订阅账号处处遮住中间部分——名字、账号行、悬停标题、勾选名、删除确认、试聊抽屉', async ({ page }) => {
  await prepare(page)
  const bob = rowOf(page, 'bob-team')
  const alice = rowOf(page, ALICE_MASKED)
  const carol = rowOf(page, CAROL_MASKED)
  // 名字（常常就是邮箱）：遮过，悬停标题也是遮过的
  await expect(alice.getByText(ALICE_MASKED, { exact: true })).toHaveAttribute('title', ALICE_MASKED)
  // 邮箱不是渠道名时另起一行的账号
  await expect(bob.getByText(BOB_MASKED, { exact: true })).toBeVisible()
  await expect(bob.locator(`[title="${BOB_MASKED}"]`)).toHaveCount(1)
  // 勾选与开关的读屏名也是遮过的
  await expect(alice.getByRole('checkbox', { name: `勾选 ${ALICE_MASKED}` })).toBeVisible()
  // 整页文字里没有任何一个完整邮箱
  const text = await page.locator('body').innerText()
  for (const raw of ['alice.dev@gmail.com', 'bob@hot-shot.com', 'carol@outlook.com']) expect(text).not.toContain(raw)
  const html = await page.locator('table').innerHTML()
  for (const raw of ['alice.dev@gmail.com', 'bob@hot-shot.com', 'carol@outlook.com']) expect(html).not.toContain(raw)

  // 试聊抽屉：标题、头部、账号下拉都是遮过的（carol 有两把 key，下拉里列账号）
  await carol.getByRole('button', { name: '更多操作', exact: true }).click()
  await page.getByRole('menuitem', { name: '测试输出', exact: true }).click()
  const drawer = page.getByRole('dialog')
  await expect(drawer).toContainText(`账号测试 · ${CAROL_MASKED}`)
  const accounts = drawer.getByLabel('账号', { exact: true })
  await expect(accounts.locator('option')).toHaveText(['按调度顺序任选可用账号', `#31 · ${CAROL_MASKED}`, '#32'])
  expect(await drawer.innerText()).not.toContain('carol@outlook.com')
  await page.keyboard.press('Escape')

  // 删除确认：标题与要手输的文字都是遮过的，看到什么输什么
  await alice.getByRole('button', { name: '更多操作', exact: true }).click()
  await page.getByRole('menuitem', { name: '删除', exact: true }).click()
  const confirm = page.getByRole('alertdialog')
  await expect(confirm).toContainText(`删除 ${ALICE_MASKED}？`)
  await expect(confirm.getByRole('button', { name: '删除', exact: true })).toBeDisabled()
  await confirm.locator('#confirm-text').fill(ALICE_MASKED)
  await expect(confirm.getByRole('button', { name: '删除', exact: true })).toBeEnabled()
  expect(await confirm.innerText()).not.toContain('alice.dev@gmail.com')
})

test('渠道列表：渠道名只有被识别为账号名才遮中间——邮箱、或等于自己 key 的账号标识；其余渠道名原样', async ({ page }) => {
  await prepare(page)
  const named = (id: number, name: string, accountLabel?: string) => ({
    ...base, id, name, provider: 'anthropic_max', api_base: 'https://api.anthropic.com', status: 1, models: ['claude-sonnet-4-5'],
    keys: [key({ id: id * 10, ...(accountLabel ? { account_label: accountLabel } : {}) })],
  })
  const rows = [
    named(11, 'dave-claude-pro', 'dave-claude-pro'), // 名字等于账号标识（账号不是邮箱）→ 遮
    named(12, 'team-shared', 'erin@hot-shot.com'), // 名字是普通名字，账号是邮箱 → 名字原样，账号行遮
    named(13, 'frank@gmail.com'), // 名字本身是邮箱（没记录账号标识）→ 遮
    named(14, 'openai-main'), // 普通渠道名 → 原样
  ]
  await page.route('**/admin/channels?*', (route) => route.fulfill({ json: { data: rows, total: rows.length, enabled: rows.length } }))
  await page.reload()
  const table = page.getByRole('table')
  await expect(table.getByText('da***ro', { exact: true })).toBeVisible()
  await expect(table.getByText('team-shared', { exact: true })).toBeVisible()
  await expect(table.getByText('e***n@hot-shot.com', { exact: true })).toBeVisible() // 另起一行的账号
  await expect(table.getByText('f***k@gmail.com', { exact: true })).toBeVisible()
  await expect(table.getByText('openai-main', { exact: true })).toBeVisible()
  const html = await table.innerHTML()
  for (const raw of ['dave-claude-pro', 'erin@hot-shot.com', 'frank@gmail.com']) expect(html, raw).not.toContain(raw)
})

test('渠道列表：所见即可点——未入池/模型/优先级/额度直达对应配置段，测活与余额原地触发，30天消费直达日志，异常一键筛选', async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  await prepare(page)
  const calls: { method: string; path: string }[] = []
  await page.route('**/admin/channels/*/test', async (route) => {
    calls.push({ method: route.request().method(), path: new URL(route.request().url()).pathname })
    await route.fulfill({ json: { ok: true, latency_ms: 210 } })
  })
  await page.route('**/admin/channels/*/balance', async (route) => {
    calls.push({ method: route.request().method(), path: new URL(route.request().url()).pathname })
    await route.fulfill({ json: { probe: 'openai', currency: 'USD', balance_micro: 15_000_000, total_micro: null, used_micro: null, at: '2026-10-23T21:00:00Z' } })
  })

  // 1. 一键「仅看异常」过滤出异常渠道（Carol 与未入池的 OpenAI）
  const attention = page.locator('[data-slot="channel-attention-filter"]')
  await expect(attention).toBeVisible()
  await expect(attention).toContainText('2')
  await attention.click()
  await expect(attention).toHaveAttribute('aria-pressed', 'true')
  await expect(page.locator('tbody tr')).toHaveCount(2)
  await attention.click()
  await expect(page.locator('tbody tr')).toHaveCount(5)

  // 2. 点击「未入池」直达「模型与渠道池」且渠道池段已展开
  const openai = rowOf(page, 'openai-main')
  await openai.getByRole('button', { name: '未入池', exact: true }).click()
  const drawer = page.getByRole('dialog')
  await expect(drawer.getByRole('tab', { name: '模型与调度池', exact: true })).toHaveAttribute('aria-selected', 'true')
  await expect(drawer.locator('#channel-pools')).toHaveAttribute('open', '')
  await page.keyboard.press('Escape')
  await expect(drawer).toHaveCount(0)

  // 3. 悬停模型数即时显示实际模型名，点击直达模型编辑
  const modelsBtn = openai.getByRole('button', { name: '3 个模型', exact: true })
  await modelsBtn.hover()
  await expect(page.getByRole('tooltip')).toContainText('gpt-5, gpt-5-mini, o4-mini')
  await modelsBtn.click()
  await expect(drawer.getByRole('tab', { name: '模型与调度池', exact: true })).toHaveAttribute('aria-selected', 'true')
  await page.keyboard.press('Escape')

  // 4. 点击优先级直达「调度」且「调度与成本设置」已展开
  await openai.getByRole('button', { name: /优先级 5/ }).click()
  await expect(drawer.getByRole('tab', { name: '调度', exact: true })).toHaveAttribute('aria-selected', 'true')
  await expect(drawer.locator('#channel-schedule-options')).toHaveAttribute('open', '')
  await page.keyboard.press('Escape')

  // 5. 点击订阅额度区直达「调度」且「额度上限」已展开
  const bob = rowOf(page, 'bob-team')
  await bob.locator('[data-slot="quota-meter"]').first().click()
  await expect(drawer.getByRole('tab', { name: '调度', exact: true })).toHaveAttribute('aria-selected', 'true')
  await expect(drawer.locator('#channel-quota-limits')).toHaveAttribute('open', '')
  await page.keyboard.press('Escape')

  // 6. 点击失败的测活结果原地重测；点击余额查询时间原地刷新余额
  const carol = rowOf(page, CAROL_MASKED)
  await carol.locator('[data-slot="last-probe"]').click()
  await expect.poll(() => calls.some((c) => c.method === 'POST' && c.path === '/admin/channels/3/test')).toBe(true)
  await openai.getByRole('button', { name: '20:45 查询' }).click()
  await expect.poll(() => calls.some((c) => c.method === 'GET' && c.path === '/admin/channels/4/balance')).toBe(true)

  // 7. 近 30 天消费直链该渠道请求日志
  const aliceUsage = rowOf(page, ALICE_MASKED).locator('[data-slot="channel-usage"]')
  await expect(aliceUsage).toHaveAttribute('href', '/admin/logs?channel_id=1')
})

test('渠道列表：置顶 / 取消置顶、横向卡片视图切换持久化、24 小时逐时成功率绿色柱与悬停明细', async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  const pinnedIds = new Set<number>()
  const pinCalls: { path: string; body: Json | null }[] = []
  const slotStart = Math.floor(nowSec / 1800) * 1800 - 47 * 1800
  const makeHourly = (pattern: (i: number) => [number, number]) =>
    Array.from({ length: 48 }, (_, i) => {
      const [requests, errors] = pattern(i)
      return { ts: slotStart + i * 1800, requests, errors }
    })
  const richHealth = {
    hourly_start: slotStart,
    data: [
      { ...HEALTH.data[0], requests: 480, errors: 0, error_rate_bp: 0, hourly: makeHourly(() => [10, 0]) },
      { ...HEALTH.data[1], requests: 536, errors: 4, error_rate_bp: 75, hourly: makeHourly((i) => (i === 36 ? [50, 1] : [12, 0])) },
      { ...HEALTH.data[2], requests: 80, errors: 12, error_rate_bp: 1500, hourly: makeHourly((i) => (i < 12 ? [0, 0] : i === 40 ? [10, 6] : i === 41 ? [25, 1] : [2, 0])) },
    ],
  }

  await prepare(page)
  await page.route('**/admin/stats/channels?*', (route) => route.fulfill({ json: richHealth }))
  await page.route('**/admin/channels?*', (route) => {
    const sorted = [...CHANNELS]
      .map((c) => ({ ...c, pinned: pinnedIds.has(Number(c.id)), pinned_at: pinnedIds.has(Number(c.id)) ? NOW.toISOString() : null }))
      .sort((a, b) => Number(b.pinned) - Number(a.pinned) || Number(b.priority) - Number(a.priority) || Number(a.id) - Number(b.id))
    return route.fulfill({ json: { data: sorted, total: sorted.length, enabled: sorted.filter((c) => c.status === 1).length } })
  })
  await page.route('**/admin/channels/*/pin', async (route) => {
    const req = route.request()
    const path = new URL(req.url()).pathname
    const id = Number(/\/admin\/channels\/(\d+)\/pin$/.exec(path)?.[1] ?? 0)
    const body = (req.postDataJSON() ?? null) as Json | null
    pinCalls.push({ path, body })
    if (body?.pinned) pinnedIds.add(id)
    else pinnedIds.delete(id)
    await route.fulfill({ json: { ok: true, pinned: pinnedIds.has(id), pinned_at: pinnedIds.has(id) ? NOW.toISOString() : null } })
  })
  await page.reload()

  // 1. 列表视图下的 24h 纤细成功率柱（全成功 48 根绿柱 / 异常混合绿黄红灰）
  const alice = rowOf(page, ALICE_MASKED)
  await expect(alice.locator('[data-slot="health-24h-bar"]')).toHaveCount(48)
  await expect(alice.locator('[data-slot="health-24h-bar"][data-state="ok"]')).toHaveCount(48)
  const carol = rowOf(page, CAROL_MASKED)
  await expect(carol.locator('[data-slot="health-24h-bar"][data-state="idle"]')).toHaveCount(12)
  await expect(carol.locator('[data-slot="health-24h-bar"][data-state="bad"]')).toHaveCount(1)
  await expect(carol.locator('[data-slot="health-24h-bar"][data-state="warn"]')).toHaveCount(1)
  await expect(carol.locator('[data-slot="health-24h-bar"][data-state="ok"]')).toHaveCount(34)

  // 2. 置顶操作：点置顶按钮发 POST /pin，渠道移到列表首位并展示置顶徽章，再点取消置顶
  const spare = rowOf(page, 'spare-compat')
  await spare.getByRole('button', { name: '置顶', exact: true }).click()
  await expect.poll(() => pinCalls.length).toBe(1)
  expect(pinCalls[0]).toEqual({ path: '/admin/channels/5/pin', body: { pinned: true } })
  const firstRow = page.locator('tbody tr').first()
  await expect(firstRow).toContainText('spare-compat')
  await expect(firstRow.locator('[data-slot="channel-pinned-badge"]')).toHaveText('置顶')
  await expect(firstRow.getByRole('button', { name: '取消置顶', exact: true })).toHaveAttribute('aria-pressed', 'true')

  // 3. 切换到横向卡片视图：多列横向并排，展示 24h 成功率大号绿柱与悬停单时段明细，刷新后保持卡片模式
  const viewToggle = page.locator('[data-slot="channel-view-toggle"]')
  await viewToggle.getByRole('button', { name: '卡片', exact: true }).click()
  const grid = page.locator('[data-slot="channel-card-grid"]')
  await expect(grid).toBeVisible()
  const cards = grid.locator('[data-slot="channel-card"]')
  await expect(cards).toHaveCount(5)
  const firstBox = (await cards.nth(0).boundingBox())!
  const secondBox = (await cards.nth(1).boundingBox())!
  // 1440px 下两张卡横向并排（第二张卡 x 明显在第一张卡右侧、y 对齐）
  expect(secondBox.x).toBeGreaterThan(firstBox.x + firstBox.width - 2)
  expect(Math.abs(secondBox.y - firstBox.y)).toBeLessThanOrEqual(2)
  // 无横向溢出
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true)

  // 悬停某根全绿时段柱：弹出结构化时段明细卡片
  const aliceCard = cards.filter({ hasText: ALICE_MASKED })
  await expect(aliceCard.locator('[data-slot="health-24h"]')).toContainText('100.0%')
  await aliceCard.locator('[data-slot="health-24h-bar"]').nth(12).hover()
  const tip = page.getByRole('tooltip')
  await expect(tip).toContainText('全部成功')
  await expect(tip).toContainText('成功 10 · 失败 0')

  // 刷新后保留卡片视图偏好
  await page.reload()
  await expect(page.locator('[data-slot="channel-card-grid"]')).toBeVisible()
  await expect(page.locator('[data-slot="channel-view-toggle"]').getByRole('button', { name: '卡片', exact: true })).toHaveAttribute('aria-pressed', 'true')

  // 在卡片视图里取消置顶并切回列表视图
  await page.locator('[data-slot="channel-card"]').first().getByRole('button', { name: '取消置顶', exact: true }).click()
  await expect.poll(() => pinCalls.length).toBe(2)
  expect(pinCalls[1]).toEqual({ path: '/admin/channels/5/pin', body: { pinned: false } })
  await page.locator('[data-slot="channel-view-toggle"]').getByRole('button', { name: '列表', exact: true }).click()
  await expect(page.locator('tbody tr')).toHaveCount(5)
})


