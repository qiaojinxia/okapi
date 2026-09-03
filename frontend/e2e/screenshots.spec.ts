import { expect, test } from '@playwright/test'
import type { APIRequestContext, Page } from '@playwright/test'

// 设计评审用截图（非断言型用例）：把主要页面在桌面宽度渲染成 PNG 供人眼/模型审阅。
// 缺省不跑——`E2E_SCREENSHOTS=1 npx playwright test screenshots` 才启用；
// 产物落 frontend/test-results/screens/，已在 .gitignore（test-results）内。
test.skip(process.env.E2E_SCREENSHOTS !== '1', '仅在 E2E_SCREENSHOTS=1 时截图')

const OUT = 'test-results/screens'

async function adminKey(
  request: APIRequestContext,
): Promise<{ apiKey: string; keyId: number } | null> {
  const login = await request.post('/auth/login', {
    headers: { 'x-real-ip': `198.51.100.${1 + Math.floor(Math.random() * 250)}` },
    data: { email: 'root@okapi.local', password: 'okapi-demo-2026' },
  })
  if (!login.ok()) return null
  const keyResp = await request.post('/auth/keys', { data: { name: `shot-${Date.now()}` } })
  if (!keyResp.ok()) return null
  const body = (await keyResp.json()) as { api_key: string; key_id: number }
  return { apiKey: body.api_key, keyId: body.key_id }
}

async function signIn(page: Page, key: string) {
  await page.goto('/')
  await page.getByRole('button', { name: 'API Key' }).click()
  await page.locator('#key').fill(key)
  await page.getByRole('button', { name: /^登录$|^Sign in$/ }).click()
  await expect(page).toHaveURL(/\/portal/)
}

/// 导航 → （可选）挂深色类 → 等数据与图表就位 → 整页截图。
/// 深色类在导航完成后加：initScript 跑在文档刚创建时，<html> 可能还不存在，加类会静默失败。
async function shot(page: Page, path: string, name: string, settle = 1_200, dark = false) {
  await page.goto(path)
  if (dark) {
    await page.evaluate(() => document.documentElement.classList.add('dark'))
  }
  await page.waitForTimeout(settle)
  await page.screenshot({ path: `${OUT}/${name}.png`, fullPage: true })
}

test.use({ viewport: { width: 1440, height: 900 } })

test('截图：门户与管理端主要页面', async ({ page, request }) => {
  // 十几张整页截图各等 1–2s 数据就位，30s 缺省预算不够；这不是断言型用例，慢一点无妨
  test.setTimeout(120_000)
  const admin = await adminKey(request)
  test.skip(admin === null, '无演示超管')
  const { apiKey, keyId } = admin as { apiKey: string; keyId: number }
  await signIn(page, apiKey)

  await shot(page, '/portal', 'portal-overview')
  await page.getByRole('tab', { name: /Token 构成|Token mix/ }).click()
  await page.waitForTimeout(400)
  await page.screenshot({ path: `${OUT}/portal-overview-tokens.png`, fullPage: true })
  await shot(page, '/portal/logs', 'portal-logs')
  await shot(page, '/portal/ledger', 'portal-ledger')

  await shot(page, '/admin', 'admin-dashboard', 2_000)
  // 渠道页：开发库有上千条测试渠道，整页图会拉成一条线——只截首屏
  await page.goto('/admin/channels')
  await page.waitForTimeout(2_000)
  await page.screenshot({ path: `${OUT}/admin-channels.png` })
  // 找出近 24h 请求最多的渠道并搜它：保证截到"近 24h"列填值的形态，而不是一列 —
  const topResp = await request.get('/admin/stats/channels?days=1&limit=1', {
    headers: { authorization: `Bearer ${apiKey}` },
  })
  const topName = ((await topResp.json()) as { data: { name: string }[] }).data[0]?.name
  if (topName) {
    await page.getByPlaceholder(/搜索|Search/).fill(topName)
    await page.waitForTimeout(800)
    // 点一次测活，让"最近测试"列有值（该测试渠道指向不可达地址 → 失败形态）
    await page.getByRole('button', { name: /^测活$|^Test$/ }).first().click()
    await page.waitForTimeout(2_500)
    await page.screenshot({ path: `${OUT}/admin-channels-health.png` })
  }
  await shot(page, '/admin/logs', 'admin-logs', 2_000)
  // 洞察三页（§11.13）：用量分析的趋势 / 堆叠 / 拆分 / 流向 / 下钻，服务质量与经营各一张
  await shot(page, '/admin/stats', 'admin-analytics-trend', 2_000)
  await shot(page, '/admin/stats?stack=model', 'admin-analytics-trend-stacked', 2_000)
  await shot(page, '/admin/stats?view=breakdown&days=30', 'admin-analytics-breakdown', 2_000)
  await shot(page, '/admin/stats?view=flow&days=30', 'admin-analytics-flow', 2_500)
  const focus = page.getByRole('main').getByRole('button', { name: /聚焦|Focus/ }).first()
  await page.goto('/admin/stats?view=breakdown&days=30')
  await page.waitForTimeout(1_800)
  if (await focus.isVisible()) {
    await focus.click()
    await page.waitForTimeout(1_800)
    await page.screenshot({ path: `${OUT}/admin-analytics-drilldown.png`, fullPage: true })
  }
  await shot(page, '/admin/quality', 'admin-quality', 2_000)
  for (const [tab, name] of [
    [/客户端分布|Client breakdown/, 'admin-quality-clients'],
    [/错误分布|Error breakdown/, 'admin-quality-errors'],
  ] as const) {
    await page.getByRole('main').getByRole('tab', { name: tab }).click()
    await page.waitForTimeout(2_000)
    await page.screenshot({ path: `${OUT}/${name}.png`, fullPage: true })
  }
  await shot(page, '/admin/revenue', 'admin-revenue', 3_000)
  // 渠道时间线抽屉：取近 7 天请求最多、有名字的渠道（id=0 是"无渠道"聚合桶）
  const busy = await request.get('/admin/stats/channels?days=7&limit=5', {
    headers: { authorization: `Bearer ${apiKey}` },
  })
  const busyName = ((await busy.json()) as { data: { name: string }[] }).data.find((c) => c.name)?.name
  if (busyName) {
    await page.goto('/admin/channels')
    await page.waitForTimeout(1_500)
    await page.getByPlaceholder(/搜索|Search/).first().fill(busyName)
    await page.waitForTimeout(1_200)
    const cell = page.getByTitle(/健康时间线|health timeline/).first()
    if (await cell.isVisible()) {
      await cell.click()
      await page.getByRole('button', { name: /近 7 天|Last 7d/ }).click()
      await page.waitForTimeout(1_800)
      await page.screenshot({ path: `${OUT}/admin-channel-timeline.png` })
    }
  }

  await page.goto('/admin/users')
  await page.locator('#u-search').fill('rootadmin')
  await page.locator('#u-search').press('Enter')
  await page
    .getByRole('main')
    .getByRole('row')
    .filter({ hasText: 'rootadmin' })
    .getByRole('button', { name: /^管理$|^Manage$/ })
    .click()
  await page.waitForTimeout(1_200)
  await page.screenshot({ path: `${OUT}/admin-user-drawer.png` })

  await page.goto('/admin/ops')
  await page.getByRole('main').getByRole('tab', { name: /死信队列|Dead-letter queue/ }).click()
  await page.waitForTimeout(1_200)
  await page.screenshot({ path: `${OUT}/admin-ops-dlq.png`, fullPage: true })

  // 审计页（§11.15）：本轮登录已落 user.login，展开第一行看键值详情
  await page.goto('/admin/audit')
  await page.waitForTimeout(1_200)
  await page.getByRole('main').getByRole('row').filter({ hasText: 'user.login' }).first().click()
  await page.waitForTimeout(400)
  await page.screenshot({ path: `${OUT}/admin-audit.png` })
  await shot(page, '/portal/security', 'portal-security')

  // 站点公告：发布一条提醒级，看登录页与门户顶部的横幅；截完即下架
  const auth = { authorization: `Bearer ${apiKey}` }
  await request.post('/admin/settings', {
    headers: auth,
    data: {
      key: 'site_notice',
      value: {
        enabled: true,
        title: '9 月 5 日 02:00–03:00 维护',
        body: '期间 API 可能出现 1–2 分钟中断。\n价格无变动。',
        level: 'warning',
        updated_at: `shot-${Date.now()}`,
      },
    },
  })
  await page.evaluate(() => localStorage.removeItem('okapi.notice.dismissed'))
  await shot(page, '/portal', 'portal-notice')
  await page.goto('/admin/settings')
  await page.getByRole('main').getByRole('tab', { name: /站点公告|Site notice/ }).click()
  await page.waitForTimeout(800)
  await page.screenshot({ path: `${OUT}/admin-settings-notice.png`, fullPage: true })
  await request.post('/admin/settings', {
    headers: auth,
    data: { key: 'site_notice', value: { enabled: false, title: '', body: '', level: 'info' } },
  })

  // 清理本轮兑的 key，别让演示超管的 key 列表随截图次数无限增长
  await request.delete(`/api/me/keys/${keyId}`, { headers: auth })
})

/// 全站交互检查：此前未评审过的页面一次截齐（管理端剩余页 + 门户剩余页 + 公开页）。
test('截图：全站剩余页面', async ({ page, request }) => {
  test.setTimeout(180_000)
  const admin = await adminKey(request)
  test.skip(admin === null, '无演示超管')
  const { apiKey, keyId } = admin as { apiKey: string; keyId: number }
  await signIn(page, apiKey)

  const pages: [string, string][] = [
    ['/admin/pools', 'admin-pools'],
    ['/admin/pricing', 'admin-pricing'],
    ['/admin/groups', 'admin-groups'],
    ['/admin/rules', 'admin-rules'],
    ['/admin/codes', 'admin-codes'],
    ['/admin/plans', 'admin-plans'],
    ['/admin/users', 'admin-users'],
    ['/admin/roles', 'admin-roles'],
    ['/admin/keys', 'admin-keys'],
    ['/admin/settings', 'admin-settings'],
    ['/admin/ops', 'admin-ops-refund'],
    ['/portal/keys', 'portal-keys'],
    ['/portal/topup', 'portal-topup'],
    ['/portal/aff', 'portal-aff'],
    ['/portal/teams', 'portal-teams'],
    ['/portal/security', 'portal-security'],
    ['/pricing', 'public-pricing'],
  ]
  for (const [path, name] of pages) {
    await page.goto(path)
    await page.waitForTimeout(1_500)
    // 列表页开发库数据量大，只截首屏（整页会拉成长条）
    await page.screenshot({ path: `${OUT}/${name}.png` })
  }
  // 渠道编辑抽屉与模型编辑抽屉：交互密度最高的两处
  await page.goto('/admin/channels')
  await page.waitForTimeout(1_500)
  await page.getByRole('button', { name: /^编辑$|^Edit$/ }).first().click()
  await page.waitForTimeout(800)
  await page.screenshot({ path: `${OUT}/admin-channel-drawer.png` })
  await page.goto('/admin/channels')
  await page.waitForTimeout(1_000)
  await page.getByRole('button', { name: /新建渠道|New channel/ }).click()
  await page.waitForTimeout(600)
  await page.screenshot({ path: `${OUT}/admin-channel-create.png` })

  await request.delete(`/api/me/keys/${keyId}`, { headers: { authorization: `Bearer ${apiKey}` } })
})

/// 深色主题：色板令牌在暗底上的对比度、硬编码色是否漏网，只有看一眼才知道。
/// 主题靠 <html class="dark">（不持久化），故用 initScript 让每次导航都带上。
test('截图：深色主题', async ({ browser, request }) => {
  const admin = await adminKey(request)
  test.skip(admin === null, '无演示超管')
  const { apiKey, keyId } = admin as { apiKey: string; keyId: number }
  const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const page = await ctx.newPage()
  await signIn(page, apiKey)

  await shot(page, '/portal', 'dark-portal-overview', 1_200, true)
  await shot(page, '/admin', 'dark-admin-dashboard', 2_000, true)
  await shot(page, '/admin/stats', 'dark-admin-stats-spend', 2_500, true)
  await page.getByRole('main').getByRole('tab', { name: /错误分布|Error breakdown/ }).click()
  await page.waitForTimeout(1_500)
  await page.screenshot({ path: `${OUT}/dark-admin-stats-errors.png`, fullPage: true })
  await shot(page, '/admin/logs', 'dark-admin-logs', 2_000, true)

  await request.delete(`/api/me/keys/${keyId}`, { headers: { authorization: `Bearer ${apiKey}` } })
  await ctx.close()
})

/// 窄屏（iPhone 尺寸）：侧栏应折叠为抽屉（§11.8），六列 KPI 应降为两列，
/// 宽表允许横向滚动但不得撑破页面。
test('截图：窄屏', async ({ browser, request }) => {
  const admin = await adminKey(request)
  test.skip(admin === null, '无演示超管')
  const { apiKey, keyId } = admin as { apiKey: string; keyId: number }
  const ctx = await browser.newContext({ viewport: { width: 390, height: 844 } })
  const page = await ctx.newPage()
  await signIn(page, apiKey)

  await shot(page, '/portal', 'm-portal-overview')
  await shot(page, '/portal/logs', 'm-portal-logs')
  await shot(page, '/admin', 'm-admin-dashboard', 2_000)
  await shot(page, '/admin/logs', 'm-admin-logs', 2_000)
  // 首屏视口版：整页图在窄屏下会拉成长条，细节看不清
  await page.screenshot({ path: `${OUT}/m-admin-logs-top.png` })
  await page.evaluate(() => window.scrollTo(0, 700))
  await page.waitForTimeout(200)
  await page.screenshot({ path: `${OUT}/m-admin-logs-table.png` })
  // 抽屉展开态
  await page.getByRole('button', { name: /打开导航|Open navigation/ }).click()
  await page.waitForTimeout(400)
  await page.screenshot({ path: `${OUT}/m-nav-drawer.png` })

  await request.delete(`/api/me/keys/${keyId}`, { headers: { authorization: `Bearer ${apiKey}` } })
  await ctx.close()
})
