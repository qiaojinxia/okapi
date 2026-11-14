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
  await expect(page.getByRole('group', { name: '表格横向浏览' })).toBeVisible()
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
  for (const [tab, name] of [['模型与渠道池', 'models'], ['调度', 'sched'], ['请求与计费行为', 'behavior']] as const) {
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

test('渠道列表：一行两层、六块各成一列——渠道 / 状态 / 额度 / 近 30 天 / 调度 / 操作', async ({ page }) => {
  // 宽屏（1608）下六块信息全部展开；窄一些时三级信息（编号、测试时间、调度文字）让位，见后面的 1280 用例
  await page.setViewportSize({ width: 1608, height: 900 })
  await prepare(page)
  const alice = rowOf(page, ALICE_MASKED)
  await expect(alice).toBeVisible()
  await expect(page.locator('thead th')).toHaveText(['', '渠道', '状态', '额度 / 余额', '近 30 天', '调度', '操作'])

  // 渠道格：第一行名称（这一行唯一的粗体）+ #ID；第二行是平台 / 档位胶囊 + 显式出口
  await expect(alice.locator('[data-slot="channel-name"]')).toHaveText(ALICE_MASKED)
  await expect(alice.locator('[data-slot="channel-name"]')).toHaveCSS('font-weight', '600')
  await expect(alice.getByText('#1', { exact: true })).toBeVisible()
  const meta = alice.locator('[data-slot="channel-meta"]')
  await expect(meta.locator('[data-slot="provider-chip"]')).toHaveText('Claude Pro')
  // 订阅渠道都打官方地址，不逐行重复；邮箱就是渠道名时也不重复账号
  await expect(alice).not.toContainText('api.anthropic.com')
  await expect(alice.locator('[data-slot="channel-account"]')).toHaveCount(0)
  await expect(alice.locator('[data-slot="channel-host"]')).toHaveCount(0)
  // 出口压缩为地球图标 + 代理名，不重复「代理」二字（类型在悬停提示里）
  await expect(meta.locator('[data-slot="channel-egress"]').getByTitle('代理 216.167.15.231', { exact: true })).toHaveText('216.167.15.231')

  // 状态格：第一行状态胶囊 + 最近测试，第二行近 24h；正常的渠道只有这两行
  const state = alice.locator('[data-slot="channel-status"]')
  await expect(state).toContainText('正常')
  await expect(state.locator('[data-slot="last-probe"]')).toContainText('1104 ms')
  await expect(state.locator('[data-slot="last-probe"]')).toContainText('10-16 22:06')
  await expect(state.locator('[data-slot="health-24h"]')).toHaveText('近 24h 0.0% 错误 · 1 次')
  await expect(state.locator('[data-slot="key-state-detail"]')).toHaveCount(0)
  // 凭证到期属于编辑详情，不占列表
  await expect(alice).not.toContainText('凭证')
  await expect(alice).not.toContainText('10-24 01:50 到期')

  // 额度格：各窗口横排成一组小仪表，窗口名用短写（全称在读屏名与悬停提示里）
  const fiveHour = alice.getByRole('progressbar', { name: '5小时额度用量' })
  const sevenDay = alice.getByRole('progressbar', { name: '7天额度用量' })
  await expect(fiveHour).toHaveAttribute('aria-valuenow', '0')
  await expect(sevenDay).toHaveAttribute('aria-valuenow', '0')
  await expect(alice.locator('[data-slot="quota-meter"]')).toHaveCount(2)
  await expect(alice.locator('[data-slot="quota-meter"]').first()).toContainText('5h')
  await expect(alice.locator('[data-slot="quota-meter"]').nth(1)).toContainText('7d')
  await expect(alice.locator('[data-slot="quota-reset"]').nth(1)).toHaveText('5天22小时')

  // 近 30 天：金额打头（加粗、不带冗余的 US 前缀），下面一行是次数与 token 总量；不再是三块带标签的小格
  const usage = alice.locator('[data-slot="channel-usage"]')
  const spend = alice.locator('[data-slot="channel-spend"]')
  await expect(spend).toHaveText('$314.68')
  await expect(spend).toHaveCSS('font-size', '14px')
  await expect(spend).toHaveCSS('font-weight', '600')
  await expect(usage).toContainText('2,085 次 · 6.4亿')
  await expect(usage).not.toContainText('30 天花费')
  await expect(alice).not.toContainText('US$314.68')

  // 调度格：是否参与调度的开关 + 优先级，下面一行是模型数与所在池
  const schedule = alice.locator('[data-slot="channel-schedule"]')
  await expect(schedule.getByRole('switch', { name: `参与调度：${ALICE_MASKED}` })).toBeChecked()
  await expect(schedule).toContainText('优先级 0')
  await expect(schedule).toContainText('2 个模型 · default')

  // 操作格：测试 / 编辑 / 更多操作三个图标按钮（名字在读屏名与悬停提示里，按钮上没有字）
  const actions = alice.getByRole('group', { name: '操作', exact: true })
  await expect(actions).toHaveAttribute('data-slot', 'action-group')
  for (const name of ['测试', '编辑', '更多操作']) {
    const button = actions.getByRole('button', { name, exact: true })
    await expect(button).toBeVisible()
    await expect(button).toHaveText('')
    await expect(button.locator('svg')).toHaveCount(1)
  }
  // 旧的六个图标按钮已收起
  await expect(alice.getByRole('button', { name: '复制', exact: true })).toHaveCount(0)
  await expect(alice.getByRole('button', { name: '删除', exact: true })).toHaveCount(0)
  await expect(alice.getByRole('button', { name: '测试输出', exact: true })).toHaveCount(0)

  // 每一格都是两行文字高：正常的行同高（此前四格各堆四五行，行高一百多且每行不同）
  const heights = []
  for (const name of [ALICE_MASKED, 'bob-team', 'spare-compat']) heights.push((await rowOf(page, name).boundingBox())!.height)
  for (const height of heights) {
    expect(height).toBeLessThanOrEqual(70)
    expect(Math.abs(height - heights[0])).toBeLessThanOrEqual(1)
  }
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

test('渠道列表：额度仪表横排，各行的同一个窗口上下对齐、等宽，重置说明在各自的条下面；勾选框与名称同高', async ({ page }) => {
  await prepare(page)
  // 三个订阅渠道各两个窗口，共 6 条。每个渠道的仪表是一排等宽的格子，所以所有行的 5h 在同一竖列、7d 在同一竖列，
  // 一眼能上下比（此前每个窗口竖着占一行，三个窗口的账号就是三四行高）
  await expect(page.getByRole('progressbar')).toHaveCount(6)
  const boxes = []
  for (const bar of await page.getByRole('progressbar').all()) boxes.push((await bar.boundingBox())!)
  const five = [boxes[0], boxes[2], boxes[4]], seven = [boxes[1], boxes[3], boxes[5]]
  for (const column of [five, seven]) {
    for (const box of column) {
      expect(Math.abs(box.x - column[0].x)).toBeLessThanOrEqual(1)
      expect(Math.abs(box.width - column[0].width)).toBeLessThanOrEqual(1)
    }
  }
  // 同一行里两个窗口并排、同高同宽
  expect(Math.abs(five[0].y - seven[0].y)).toBeLessThanOrEqual(1)
  expect(Math.abs(five[0].width - seven[0].width)).toBeLessThanOrEqual(1)
  expect(seven[0].x).toBeGreaterThan(five[0].x + five[0].width)
  // 重置说明在各自那条的正下方，左缘与条对齐
  const bob = rowOf(page, 'bob-team')
  const resets = bob.locator('[data-slot="quota-reset"]')
  await expect(resets).toHaveText(['15分钟', '5天10小时'])
  const bars = [(await bob.getByRole('progressbar').nth(0).boundingBox())!, (await bob.getByRole('progressbar').nth(1).boundingBox())!]
  for (const index of [0, 1]) {
    const reset = (await resets.nth(index).boundingBox())!
    expect(Math.abs(reset.x - bars[index].x)).toBeLessThanOrEqual(1)
    expect(reset.y).toBeGreaterThan(bars[index].y)
    expect(reset.x + reset.width).toBeLessThanOrEqual(bars[index].x + bars[index].width + 1)
  }
  // 勾选框与名称首行垂直居中对齐（行内框落在基线上时会比名字高出两三像素）
  const alice = rowOf(page, ALICE_MASKED)
  const checkbox = (await alice.getByRole('checkbox').boundingBox())!
  const name = (await alice.getByText(ALICE_MASKED, { exact: true }).first().boundingBox())!
  expect(Math.abs(checkbox.y + checkbox.height / 2 - (name.y + name.height / 2))).toBeLessThanOrEqual(1.5)
})

test('渠道列表：部分 key 不可用时状态胶囊转黄并多一行计数 / 恢复时间；停用与未入池各有标识', async ({ page }) => {
  await prepare(page)
  const carol = rowOf(page, CAROL_MASKED)
  const status = carol.locator('[data-slot="channel-status"]')
  const first = status.locator('> div').first()
  await expect(first).toContainText('1/2 可用')
  // 最近一次测试失败：写错误码而不是延迟
  await expect(status.locator('[data-slot="last-probe"]')).toContainText('upstream_timeout')
  // 计数与恢复时间自成一行（只有异常的渠道才有这一行），不挤进第一行的「错误码 + 时间」里
  const detail = status.locator('[data-slot="key-state-detail"]')
  await expect(detail).toContainText('冷却 1')
  await expect(detail).toContainText('约 20 分钟后恢复')
  await expect(detail).not.toContainText('upstream_timeout')
  await expect(first).not.toContainText('冷却 1')
  const health = status.locator('[data-slot="health-24h"]')
  await expect(health).toContainText('15.0% 错误 · 80 次')
  // 自上而下三层：状态 → 原因 → 健康
  const ys = []
  for (const part of [first, detail, health]) ys.push((await part.boundingBox())!.y)
  expect(ys[0]).toBeLessThan(ys[1])
  expect(ys[1]).toBeLessThan(ys[2])
  await expect(carol).not.toContainText('已过期')
  // 异常的行比正常的行高出一行，本身就是信号
  expect((await carol.boundingBox())!.height).toBeGreaterThan((await rowOf(page, 'bob-team').boundingBox())!.height + 12)

  const openai = rowOf(page, 'openai-main')
  await expect(openai.locator('[data-slot="channel-status"]')).toContainText('未入池')
  // API 渠道第二行：平台胶囊（「API Key」缩成「API」）+ 上游主机名 + 显式出口
  await expect(openai.locator('[data-slot="provider-chip"]')).toHaveText('OpenAI API')
  await expect(openai.locator('[data-slot="channel-host"]')).toHaveText('api.openai.com')
  await expect(openai.locator('[data-slot="channel-host"]')).toHaveAttribute('title', 'https://api.openai.com/v1')
  await expect(openai.locator('[data-slot="channel-egress"]')).toContainText('直连')
  // 上游余额在「额度 / 余额」这一列：金额打头带标签，下面是查询时间；和右边的计费金额不混在一起
  const balance = openai.locator('[data-slot="channel-balance"]')
  await expect(balance).toContainText('$12.34')
  await expect(balance).toContainText('余额')
  await expect(balance).toContainText('20:45 查询')
  // 近 30 天没有计费调用：一个破折号（说明留给读屏与悬停），不占一条灰带
  const usage = openai.locator('[data-slot="channel-usage"]')
  await expect(usage).toHaveText('—近 30 天暂无计费调用')
  await expect(usage).toHaveAttribute('title', '近 30 天暂无计费调用')

  const spare = rowOf(page, 'spare-compat')
  await expect(spare.locator('[data-slot="channel-status"] > div').first()).toContainText('停用')
  await expect(spare.getByRole('switch', { name: '参与调度：spare-compat' })).not.toBeChecked()
  await expect(spare).toContainText('未测过')
  await expect(spare).toContainText('无请求')
  // 停用的渠道整体退后一档（名字变灰），也不去查额度
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
  // openai 有余额接口：五项都在；删除排在分隔线之后
  await expect(menu.getByRole('menuitem')).toHaveText(['测试输出', '查上游余额', '额度上限', '复制', '删除'])
  // 打开后焦点落在第一项
  await expect(menu.getByRole('menuitem', { name: '测试输出', exact: true })).toBeFocused()
  await page.keyboard.press('ArrowDown')
  await expect(menu.getByRole('menuitem', { name: '查上游余额', exact: true })).toBeFocused()
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
  await expect(page.getByRole('menu').getByRole('menuitem')).toHaveText(['测试输出', '额度上限', '复制', '删除'])
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

test('渠道列表：窗口缩放时「···」菜单收起；窄屏表格横向滚动而不是把列挤坏', async ({ page }) => {
  await prepare(page)
  await rowOf(page, ALICE_MASKED).getByRole('button', { name: '更多操作', exact: true }).click()
  await expect(page.getByRole('menu')).toBeVisible()
  await page.setViewportSize({ width: 1100, height: 800 })
  await expect(page.getByRole('menu')).toHaveCount(0)

  await page.setViewportSize({ width: 800, height: 800 })
  const viewport = page.locator('[data-slot="table-viewport"]')
  await expect.poll(async () => viewport.evaluate((el) => el.scrollWidth > el.clientWidth)).toBe(true)
  await expect(page.getByRole('group', { name: '表格横向浏览' })).toBeVisible()
  // 不会把整页撑出横向滚动条
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true)
})

test('渠道列表：1280 宽下六列放得下，不出现横向滚动；三级信息让位，内容不越出自己的格子', async ({ page }) => {
  await prepare(page)
  const viewport = page.locator('[data-slot="table-viewport"]')
  await expect(viewport).toBeVisible()
  expect(await viewport.evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true)
  await expect(page.getByRole('group', { name: '表格横向浏览' })).toHaveCount(0)
  await expect(page.locator('thead th')).toHaveText(['', '渠道', '状态', '额度 / 余额', '近 30 天', '调度', '操作'])
  const alice = rowOf(page, ALICE_MASKED)
  // 窄窗口：编号、测试时间、调度文字、token 总量收起（悬停提示与编辑抽屉里都有），开关与动作还在
  await expect(alice.getByText('#1', { exact: true })).toBeHidden()
  await expect(alice.locator('[data-slot="last-probe"]')).toHaveText('1104 ms10-16 22:06')
  await expect(alice.locator('[data-slot="last-probe"] span').last()).toBeHidden()
  await expect(alice.locator('[data-slot="channel-schedule"]').getByText('优先级')).toBeHidden()
  await expect(alice.locator('[data-slot="channel-schedule"]').getByRole('switch')).toBeVisible()
  await expect(alice.getByRole('group', { name: '操作', exact: true })).toBeVisible()
  // 定宽布局：每一格的内容都截断在自己的列里，不会画到隔壁列上
  for (const name of [ALICE_MASKED, 'bob-team', CAROL_MASKED, 'openai-main']) {
    const overflow = await rowOf(page, name).evaluate((row) => [...row.querySelectorAll('td')].map((cell) => {
      const box = cell.getBoundingClientRect()
      return Math.max(0, ...[...cell.querySelectorAll('*')].filter((node) => !node.classList.contains('sr-only') && node.getClientRects().length > 0)
        .map((node) => { const inner = node.getBoundingClientRect(); return inner.width === 0 ? 0 : inner.right - box.right }))
    }))
    for (const extra of overflow) expect(extra).toBeLessThanOrEqual(1)
  }
})

test('渠道列表：宽屏（1608）下状态逐行对齐，金额右缘逐行对齐，调度文字展开', async ({ page }) => {
  await page.setViewportSize({ width: 1608, height: 900 })
  await prepare(page)
  const viewport = page.locator('[data-slot="table-viewport"]')
  expect(await viewport.evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true)
  const alice = rowOf(page, ALICE_MASKED)
  const bob = rowOf(page, 'bob-team')
  const carol = rowOf(page, CAROL_MASKED)
  // 状态格：胶囊等宽（「正常」「1/2 可用」宽度本来不同），所以右边那段文字的起点逐行一致
  const starts = []
  for (const row of [alice, bob, carol]) starts.push((await row.locator('[data-slot="last-probe"]').boundingBox())!.x)
  expect(Math.max(...starts) - Math.min(...starts)).toBeLessThanOrEqual(1)
  // 额度条够长：两个窗口时每条一百多像素
  const bar = (await alice.getByRole('progressbar', { name: '5小时额度用量' }).boundingBox())!
  expect(bar.width).toBeGreaterThanOrEqual(100)
  // 近 30 天：金额在上、次数与 token 在下，整格右对齐——各行金额的右缘在一条线上
  const amount = (await alice.locator('[data-slot="channel-spend"]').boundingBox())!
  const other = (await bob.locator('[data-slot="channel-spend"]').boundingBox())!
  expect(Math.abs(amount.x + amount.width - (other.x + other.width))).toBeLessThanOrEqual(1)
  const requests = (await alice.getByText(/2,085 次/).boundingBox())!
  expect(requests.y).toBeGreaterThan(amount.y + amount.height - 2)
  expect(Math.abs(requests.x + requests.width - (amount.x + amount.width))).toBeLessThanOrEqual(1)
  // 状态首行文字的末端与额度格的左缘之间留得出空隙
  const probe = (await alice.locator('[data-slot="last-probe"]').boundingBox())!
  expect(bar.x - (probe.x + probe.width)).toBeGreaterThanOrEqual(12)
  // 窗口够宽：调度文字展开
  await expect(alice.locator('[data-slot="channel-schedule"]').getByText('优先级')).toBeVisible()
})

test('渠道列表：深色主题下状态点与额度条仍用语义色', async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem('okapi.theme', 'dark'))
  await prepare(page)
  await expect(page.locator('html')).toHaveClass(/dark/)
  const carol = rowOf(page, CAROL_MASKED)
  await expect(carol.locator('[data-slot="channel-status"] span').first()).toHaveClass(/text-warning/)
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
    expect(box.width).toBe(32)
    expect(box.height).toBe(32)
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

