import { expect, test } from '@playwright/test'
import type { Page } from '@playwright/test'
import { fileURLToPath } from 'node:url'

// 渠道列表的版面与行内交互：状态分段筛选、「···」行菜单、行内五块信息（渠道 / 状态 / 用量窗口 / 调度操作）。
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

test('渠道列表：页头统计是朴素文字，按钮无图标，新建渠道的加号只是装饰', async ({ page }) => {
  await prepare(page)
  const header = page.locator('[data-slot="page-header"]')
  await expect(header).toContainText('共 5 条 · 4 启用')
  // 统计不再是徽章胶囊，页头也没有图标方块
  await expect(header.locator('[class*="rounded-full"]')).toHaveCount(0)
  await expect(header.locator('svg')).toHaveCount(0)
  // 加号用 aria-hidden 的文字画，读屏与按名字找按钮时只有「新建渠道」
  await expect(header.getByRole('button', { name: '新建渠道', exact: true })).toBeVisible()
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

test('渠道列表：一行五块信息——渠道 / 状态 / 用量窗口 / 调度与操作', async ({ page }) => {
  await prepare(page)
  const alice = rowOf(page, 'alice.dev@gmail.com')
  await expect(alice).toBeVisible()

  // 渠道格：#ID + 名称、地址、平台 + 档位胶囊、模型数与优先级
  await expect(alice.getByText('#1', { exact: true })).toBeVisible()
  await expect(alice.getByText('https://api.anthropic.com', { exact: true })).toBeVisible()
  await expect(alice.getByText('Claude Pro', { exact: true })).toBeVisible()
  await expect(alice.getByText('2 个模型 · 优先级 0', { exact: true })).toBeVisible()
  // 订阅账号邮箱就是渠道名时不再另起一行重复
  await expect(alice.getByText('账号', { exact: true })).toHaveCount(0)

  // 状态格：状态点 + 最近测试同一行，下面是带标签的明细
  const state = alice.locator('dl')
  await expect(state.locator('dt').first()).toHaveText('正常')
  await expect(state.locator('dd').first()).toContainText('1104 ms')
  await expect(state.locator('dd').first()).toContainText('10-16 22:06')
  await expect(state).toContainText('近 24h')
  await expect(state).toContainText('0.0% 错误 · 1 次')
  await expect(state).toContainText('凭证')
  await expect(state).toContainText('10-24 01:50 到期')
  await expect(state).toContainText('出口')
  await expect(state).toContainText('代理 216.167.15.231')
  // 旧的「测试」标签行已并进第一行
  await expect(state.locator('dt', { hasText: /^测试$/ })).toHaveCount(0)

  // 用量窗口：计数一行（金额是重点）+ 额度窗口条
  await expect(alice).toContainText('2,085 次 · 6.4亿 tokens · US$314.68')
  const fiveHour = alice.getByRole('progressbar', { name: '5小时额度用量' })
  const sevenDay = alice.getByRole('progressbar', { name: '7天额度用量' })
  await expect(fiveHour).toHaveAttribute('aria-valuenow', '0')
  await expect(sevenDay).toHaveAttribute('aria-valuenow', '0')
  await expect(alice).toContainText('5天22小时')

  // 调度开关在上，测试 / 编辑文字按钮与「更多操作」在下
  await expect(alice.getByRole('switch', { name: '参与调度：alice.dev@gmail.com' })).toBeChecked()
  await expect(alice.getByRole('button', { name: '测试', exact: true })).toBeVisible()
  await expect(alice.getByRole('button', { name: '编辑', exact: true })).toBeVisible()
  await expect(alice.getByRole('button', { name: '更多操作', exact: true })).toBeVisible()
  // 旧的六个图标按钮已收起
  await expect(alice.getByRole('button', { name: '复制', exact: true })).toHaveCount(0)
  await expect(alice.getByRole('button', { name: '删除', exact: true })).toHaveCount(0)
  await expect(alice.getByRole('button', { name: '测试输出', exact: true })).toHaveCount(0)
})

test('渠道列表：额度条按窗口着色，近上限转黄转红；账号与渠道名不同时补一行账号', async ({ page }) => {
  await prepare(page)
  const bob = rowOf(page, 'bob-team')
  await expect(bob).toContainText('Claude Max 5x')
  // 邮箱不是渠道名：补一行「账号」
  await expect(bob.getByText('账号', { exact: true })).toBeVisible()
  await expect(bob).toContainText('bob@hot-shot.com')
  const bobFive = bob.getByRole('progressbar', { name: '5小时额度用量' })
  const bobSeven = bob.getByRole('progressbar', { name: '7天额度用量' })
  await expect(bobFive).toHaveAttribute('aria-valuenow', '55')
  await expect(bobSeven).toHaveAttribute('aria-valuenow', '18')
  await expect(bobFive.locator('div')).toHaveClass(/bg-amber-700/)
  await expect(bobSeven.locator('div')).toHaveClass(/bg-primary/)
  await expect(bob).toContainText('15分钟')

  const carol = rowOf(page, 'carol@outlook.com')
  await expect(carol.getByRole('progressbar', { name: '5小时额度用量' }).locator('div')).toHaveClass(/bg-destructive/)
  await expect(carol.getByRole('progressbar', { name: '7天额度用量' }).locator('div')).toHaveClass(/bg-warning/)
})

test('渠道列表：部分 key 不可用时状态点转黄并补计数 / 恢复时间；停用与未入池各有标识', async ({ page }) => {
  await prepare(page)
  const carol = rowOf(page, 'carol@outlook.com')
  await expect(carol.locator('dt').first()).toHaveText('1/2 可用')
  await expect(carol).toContainText('冷却 1')
  await expect(carol).toContainText('约 20 分钟后恢复')
  // 最近一次测试失败：写错误码而不是延迟
  await expect(carol.locator('dd').first()).toContainText('upstream_timeout')
  await expect(carol).toContainText('15.0% 错误 · 80 次')
  await expect(carol).toContainText('已过期')

  const openai = rowOf(page, 'openai-main')
  await expect(openai).toContainText('未入池')
  await expect(openai).toContainText('直连')
  // 上游余额：带标签，不和上面的计费金额混在一起
  await expect(openai).toContainText('余额')
  await expect(openai).toContainText('US$12.34')
  await expect(openai.getByText('OpenAI API Key', { exact: true })).toBeVisible()

  const spare = rowOf(page, 'spare-compat')
  await expect(spare.locator('dt').first()).toHaveText('停用')
  await expect(spare.getByRole('switch', { name: '参与调度：spare-compat' })).not.toBeChecked()
  await expect(spare).toContainText('未测过')
  await expect(spare).toContainText('无请求')
  // 停用的渠道不去查额度
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

test('渠道列表：「···」菜单收纳试聊 / 余额 / 复制 / 删除，键盘可开可关', async ({ page }) => {
  await prepare(page)
  const openai = rowOf(page, 'openai-main')
  const more = openai.getByRole('button', { name: '更多操作', exact: true })
  await expect(more).toHaveAttribute('aria-haspopup', 'menu')
  await expect(more).toHaveAttribute('aria-expanded', 'false')
  await more.click()
  const menu = page.getByRole('menu')
  await expect(menu).toBeVisible()
  await expect(more).toHaveAttribute('aria-expanded', 'true')
  // openai 有余额接口：四项都在；删除排在分隔线之后
  await expect(menu.getByRole('menuitem')).toHaveText(['测试输出', '查上游余额', '复制', '删除'])
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
  const alice = rowOf(page, 'alice.dev@gmail.com')
  await alice.getByRole('button', { name: '更多操作', exact: true }).click()
  await expect(page.getByRole('menu').getByRole('menuitem')).toHaveText(['测试输出', '复制', '删除'])
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
  await rowOf(page, 'alice.dev@gmail.com').getByRole('button', { name: '更多操作', exact: true }).click()
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

test('渠道列表：1280 宽下五列放得下，不出现横向滚动', async ({ page }) => {
  await prepare(page)
  const viewport = page.locator('[data-slot="table-viewport"]')
  await expect(viewport).toBeVisible()
  expect(await viewport.evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true)
  await expect(page.getByRole('group', { name: '表格横向浏览' })).toHaveCount(0)
  // 表头五列：勾选 / 渠道 / 状态 / 用量窗口 / 调度 / 操作
  await expect(page.locator('thead th')).toHaveText(['', '渠道', '状态', '用量窗口', '调度 / 操作'])
})

test('渠道列表：深色主题下状态点与额度条仍用语义色', async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem('okapi.theme', 'dark'))
  await prepare(page)
  await expect(page.locator('html')).toHaveClass(/dark/)
  const carol = rowOf(page, 'carol@outlook.com')
  await expect(carol.locator('dt span').first()).toHaveClass(/text-warning/)
  await expect(carol.getByRole('progressbar', { name: '5小时额度用量' }).locator('div')).toHaveClass(/bg-destructive/)
})
