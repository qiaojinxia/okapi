import { expect, test } from '@playwright/test'
import type { Page } from '@playwright/test'
import { fileURLToPath } from 'node:url'
import type { LogStats } from '../src/features/logs/types'
import { tokenTrendPoints } from '../src/features/portal-keys/KeyUsageSparkline'

const stats: LogStats = {
  records: 77, settled: 73, failed: 3, refunded: 1, pending: 0,
  amount_micro: 869, refunded_amount_micro: 202,
  prompt_tokens: 90000, completion_tokens: 12000, cached_tokens: 2000, cache_read_samples: 74,
  avg_latency_ms: 2400, latency_samples: 73, avg_ttft_ms: 120, ttft_samples: 70,
}
const emptyStats: LogStats = {
  records: 0, settled: 0, failed: 0, refunded: 0, pending: 0,
  amount_micro: 0, refunded_amount_micro: 0,
  prompt_tokens: 0, completion_tokens: 0, cached_tokens: 0, cache_read_samples: 0,
  avg_latency_ms: null, latency_samples: 0, avg_ttft_ms: null, ttft_samples: 0,
}
const keys = [42, 43].map((id) => ({
  id, name: 'production', key_prefix: `sk-fixture-${id}`, status: id === 42 ? 1 : 2,
  used_micro: 100, amount_micro: 100, requests: 1, rpm_limit: null,
  created_at: '2026-09-28T00:00:00Z', group_override: null, ip_allowlist: null,
  usage_trend: {
    days: ['2026-09-22', '2026-09-23', '2026-09-24', '2026-09-25', '2026-09-26', '2026-09-27', '2026-09-28'],
    tokens: id === 42 ? [0, 150, 20, 500, 320, 1000, 400] : [0, 0, 0, 0, 0, 0, 0],
    timezone: 'UTC',
  },
}))
const log = {
  id: 99, request_id: 'aaaaaaaa-0000-4000-8000-000000000042', api_key_id: 42, key_name: 'production',
  model: 'fixture-model', log_type: 2, status: 20, is_stream: true,
  usage: { prompt_tokens: 1000, completion_tokens: 500, cached_tokens: 200, reasoning_tokens: 0,
    cache_read_reported: true, cache_write_reported: true, cache_write_tokens: 0 },
  amount_micro: 869, original_amount_micro: 869, discount_micro: 0, pricing_snapshot: null,
  error_code: null, latency_ms: 2400, ttft_ms: 120, created_at: '2026-09-28T12:00:00Z',
}

async function prepare(page: Page, mode: 'account' | 'key' | 'legacy' = 'account', language = 'zh-CN') {
  const queries: URL[] = []
  await page.addInitScript(({ mode, language }) => {
    localStorage.setItem('okapi.key', 'key-usage-fixture')
    if (mode !== 'legacy') localStorage.setItem('okapi.login-mode', mode)
    localStorage.setItem('okapi.usage-scope', 'key')
    localStorage.setItem('okapi.lang', language)
    localStorage.setItem('okapi.theme', language === 'en' ? 'dark' : 'light')
  }, { mode, language })
  await page.emulateMedia({ reducedMotion: 'reduce' })
  await page.route('**/*', (route) => {
    const request = route.request(), url = new URL(request.url())
    if (request.isNavigationRequest()) return route.fulfill({ path: fileURLToPath(new URL('../dist/index.html', import.meta.url)), contentType: 'text/html' })
    if (!/^\/(api|admin|auth)\//.test(url.pathname)) return route.continue()
    expect(request.method(), '查看统计不应产生写请求').toBe('GET')
    queries.push(url)
    const responses: Record<string, unknown> = {
      '/api/me': { user_id: 7, key_id: 1, has_web_session: mode !== 'key', role: 1, permissions: [], balance_micro: 20000000, group: 'default' },
      '/api/notice': { notice: null }, '/api/pricing': { models: [], groups: [] },
      '/api/me/keys': { data: keys, total: keys.length }, '/api/me/groups': { data: [] },
      '/api/me/logs/stat': url.searchParams.get('api_key_id') === '42' ? stats : emptyStats,
      '/api/me/logs': { data: [log], scope: 'user', next_before: null },
    }
    return route.fulfill({ json: responses[url.pathname] ?? { data: [] } })
  })
  return queries
}

test('密钥折线坐标：固定七天、零基线与大数范围稳定，不制造虚假波动', () => {
  expect(tokenTrendPoints([0, 0, 0, 0, 0, 0, 0]).split(' ').every((point) => point.endsWith(',25'))).toBe(true)
  expect(tokenTrendPoints([5, 5, 5, 5, 5, 5, 5]).split(' ').every((point) => point.endsWith(',3'))).toBe(true)
  const points = tokenTrendPoints([0, 1, 999, 1_000_000_000, 50, 0, 200]).split(' ').map((p) => p.split(',').map(Number))
  expect(points).toHaveLength(7)
  expect(points[0]).toEqual([3, 25])
  expect(points[3][1]).toBe(3)
  for (const [x, y] of points) {
    expect(x).toBeGreaterThanOrEqual(3)
    expect(x).toBeLessThanOrEqual(101)
    expect(y).toBeGreaterThanOrEqual(3)
    expect(y).toBeLessThanOrEqual(25)
  }
})

for (const { width, language } of [{ width: 1920, language: 'zh-CN' }, { width: 1440, language: 'en' }, { width: 1024, language: 'zh-CN' }]) {
  test(`密钥行内折线 ${width} ${language}：固定列宽和行高，零与缺失区别，点击打开对应统计`, async ({ page }) => {
    const queries = await prepare(page, 'account', language)
    await page.setViewportSize({ width, height: 900 })
    await page.route('**/api/me/keys?*', (route) => route.fulfill({ json: {
      data: [...keys, { ...keys[0], id: 44, usage_trend: null }, { ...keys[0], id: 45, usage_trend: { timezone: 'UTC', days: [] } }], total: 4,
    } }))
    await page.goto('/portal/keys')
    const charts = page.locator('[data-slot="key-usage-sparkline"]')
    await expect(charts).toHaveCount(4)
    await expect(charts.nth(0)).toHaveAttribute('data-state', 'ready')
    await expect(charts.nth(0).locator('[data-slot="key-trend-total"]')).toHaveText('2,390')
    await expect(charts.nth(1)).toHaveAttribute('data-state', 'empty')
    for (const i of [2, 3]) {
      await expect(charts.nth(i)).toHaveAttribute('data-state', 'unavailable')
      await expect(charts.nth(i).locator('svg')).toHaveCount(0)
    }
    const table = page.getByRole('table', { name: language === 'en' ? 'Keys' : '密钥', exact: true })
    await expect(table.locator('thead th').first()).toHaveText('Token')
    await expect(table.locator('thead th').nth(1)).toHaveText(language === 'en' ? 'Name' : '名称')
    await expect(table.getByRole('columnheader', { name: language === 'en' ? 'Prefix' : '前缀', exact: true })).toHaveCount(0)
    await expect(table.getByRole('columnheader', { name: language === 'en' ? 'Tokens · 7 days' : '近 7 天 Token', exact: true })).toBeVisible()
    for (const row of await table.locator('tbody tr').all()) expect((await row.boundingBox())!.height).toBe(56)
    for (const chart of await charts.all()) {
      expect((await chart.boundingBox())!.width).toBeGreaterThanOrEqual(120)
      expect((await chart.boundingBox())!.width).toBeLessThanOrEqual(160)
      expect((await chart.boundingBox())!.height).toBe(32)
    }
    if (width >= 1440) {
      expect(await page.locator('[data-slot="table-viewport"]').evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true)
      await expect(table.getByRole('columnheader').last()).toBeInViewport({ ratio: 1 })
    }
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    await charts.first().focus()
    await expect(page.getByRole('tooltip')).toContainText('2,390')
    await expect(page.getByRole('tooltip')).toContainText('09-27: 1,000')
    await expect(page.getByRole('tooltip')).toContainText('UTC')
    await page.keyboard.press('Escape')
    await charts.first().press('Enter')
    const dialog = page.getByRole('dialog', { name: language === 'en' ? 'Key usage overview' : '密钥用量概览' })
    await expect(dialog).toContainText('#42')
    await expect.poll(() => queries.filter((url) => url.pathname === '/api/me/logs/stat').length).toBe(1)
    await dialog.locator('header').getByRole('button').click()
    await charts.nth(1).click()
    await expect(dialog).toContainText('#43')
    await dialog.locator('header').getByRole('button').click()
    await page.getByRole('main').getByRole('heading', { name: language === 'en' ? 'Keys' : '密钥', exact: true }).click()
    await page.screenshot({ path: `test-results/key-sparkline-${width}-${language}.png`, animations: 'disabled' })
  })
}

for (const mode of ['account', 'key', 'legacy'] as const) {
  test(`密钥用量 ${mode}：按需加载独立汇总，Token 指标跳转保留准确密钥与统计范围`, async ({ page }) => {
    const queries = await prepare(page, mode)
    await page.goto('/portal/keys')
    const opener = page.getByRole('button', { name: '查看 production（#42）用量统计', exact: true })
    await expect(opener).toBeVisible()
    expect(queries.filter((url) => url.pathname === '/api/me/logs/stat')).toHaveLength(0)
    await opener.focus()
    await page.keyboard.press('Enter')
    const dialog = page.getByRole('dialog', { name: '密钥用量概览' })
    await expect(dialog).toContainText('#42')
    await expect(dialog).toContainText('全部日期 · 仅当前密钥')
    await expect(dialog).toContainText('US$0.000869')
    await expect(dialog).toContainText('已排除退款 US$0.000202')
    await expect(dialog).toContainText('结算 73 · 失败 3 · 退款 1')
    await expect(dialog).toContainText('已上报 74 / 77 条；缺失不作零')
    await expect(dialog).toContainText('120 ms')
    const requests = queries.filter((url) => url.pathname === '/api/me/logs/stat')
    expect(requests).toHaveLength(1)
    expect(Object.fromEntries(requests[0].searchParams)).toEqual({ scope: 'user', api_key_id: '42' })

    await page.keyboard.press('Escape')
    await expect(dialog).toHaveCount(0)
    await expect(opener).toBeFocused()
    await opener.click()
    await dialog.getByRole('button', { name: /输入 Token.*90,000/ }).click()
    await expect(page).toHaveURL(/\/portal\/logs\?scope=user&api_key_id=42$/)
    await expect(page.getByRole('combobox', { name: '筛选密钥', exact: true })).toHaveValue('42')
    await expect(page.getByRole('region', { name: '筛选范围汇总' })).toContainText('90,000')
    const latest = queries.filter((url) => url.pathname === '/api/me/logs' || url.pathname === '/api/me/logs/stat').slice(-2)
    expect(latest.map((url) => url.pathname).sort()).toEqual(['/api/me/logs', '/api/me/logs/stat'])
    for (const url of latest) {
      expect(url.searchParams.get('scope')).toBe('user')
      expect(url.searchParams.get('api_key_id')).toBe('42')
      expect(url.searchParams.has('start_date')).toBe(false)
    }
    await page.reload()
    await expect(page.getByRole('combobox', { name: '筛选密钥', exact: true })).toHaveValue('42')
    await page.getByRole('button', { name: `展开 ${log.request_id} 的明细`, exact: true }).click()
    await expect(page.getByRole('dialog', { name: '请求与账单详情' })).toContainText('缓存读取')
  })
}

test('同名与停用密钥：切换后无旧数据串入，空记录与未采集不伪装为零', async ({ page }) => {
  const queries = await prepare(page)
  let release!: () => void
  const gate = new Promise<void>((resolve) => { release = resolve })
  await page.route('**/api/me/logs/stat?*', async (route) => {
    if (new URL(route.request().url()).searchParams.get('api_key_id') !== '42') return route.fallback()
    await gate
    return route.fulfill({ json: stats })
  })
  await page.goto('/portal/keys')
  await page.getByRole('button', { name: '查看 production（#42）用量统计' }).click()
  const dialog = page.getByRole('dialog', { name: '密钥用量概览' })
  await expect(dialog.getByRole('region', { name: '筛选范围汇总' })).toHaveAttribute('aria-busy', 'true')
  await expect(dialog).not.toContainText('US$0.00')
  await page.keyboard.press('Escape')
  await page.getByRole('button', { name: '查看 production（#43）用量统计' }).click()
  await expect(dialog).toContainText('#43')
  await expect(dialog).toContainText('停用')
  await expect(dialog).toContainText('这把密钥还没有用量记录')
  release()
  await expect(dialog.getByRole('button', { name: /^缓存读取/ })).toContainText('—')
  await expect(dialog.getByRole('button', { name: /^平均首字延迟/ })).toContainText('—')
  await expect(dialog).not.toContainText('90,000')
  await dialog.getByRole('button', { name: '查看 Token 明细' }).click()
  await expect(page).toHaveURL(/api_key_id=43$/)
  await expect.poll(() => queries.filter((url) => url.pathname === '/api/me/logs').at(-1)?.searchParams.get('api_key_id')).toBe('43')
})

for (const width of [1024, 1440, 1920]) test(`密钥列表 ${width}：紧凑名称列、桌面完整展示、窄屏 Token 首列固定与统一行高`, async ({ page }) => {
  await prepare(page)
  await page.setViewportSize({ width, height: 900 })
  const longName = 'production-客服机器人-上海团队-长名称用于验证列宽和省略'.repeat(3)
  await page.route('**/api/me/keys?*', (route) => route.fulfill({ json: {
    total: 3, data: [
      { ...keys[0], name: 'web' },
      { ...keys[0], id: 44, name: longName, group_override: 'vip', model_allowlist: ['model-a', 'model-b'], ip_allowlist: ['10.0.0.0/8'], expires_at: '2030-10-01T12:30:00Z', requests: 999999999999, rpm_limit: 123456789 },
      { ...keys[1], name: '222' },
    ],
  } }))
  await page.goto('/portal/keys')
  const rows = page.locator('tbody tr')
  await expect(rows).toHaveCount(3)
  const tokenCell = rows.nth(1).locator('td').first()
  const nameCell = rows.nth(1).locator('td').nth(1)
  expect((await tokenCell.boundingBox())!.width).toBeGreaterThanOrEqual(160)
  expect((await nameCell.boundingBox())!.width).toBeGreaterThanOrEqual(132)
  expect((await nameCell.boundingBox())!.width).toBeLessThanOrEqual(200)
  await expect(nameCell).toHaveAttribute('title', longName)
  const label = nameCell.getByRole('button').locator('span')
  expect(await label.evaluate((el) => el.scrollWidth > el.clientWidth)).toBe(true)
  await expect(page.getByRole('columnheader', { name: '名称', exact: true })).toHaveCSS('text-align', 'left')
  await expect(page.getByRole('columnheader', { name: '有效期', exact: true })).toHaveCSS('text-align', 'left')
  await expect(page.getByRole('columnheader', { name: '消费 / 额度', exact: true })).toHaveCSS('text-align', 'left')
  await expect(page.getByRole('columnheader', { name: '累计请求', exact: true })).toHaveCSS('text-align', 'left')
  await expect(page.getByRole('columnheader', { name: 'RPM 上限', exact: true })).toHaveCSS('text-align', 'left')
  const actionsHeader = page.getByRole('columnheader', { name: '操作', exact: true })
  await expect(actionsHeader).toHaveCSS('text-align', 'center')
  const headerBounds = (await actionsHeader.boundingBox())!
  for (const row of await rows.all()) {
    expect((await row.boundingBox())!.height).toBe(56)
    await expect(row.locator('td').nth(4)).toHaveCSS('text-align', 'left')
    await expect(row.locator('td').nth(5)).toHaveCSS('text-align', 'left')
    await expect(row.locator('td').nth(6)).toHaveCSS('text-align', 'left')
    const cell = row.locator('td').nth(1)
    await expect(cell).toHaveCSS('text-align', 'left')
    const rect = (await cell.boundingBox())!
    const content = (await cell.locator(':scope > div').boundingBox())!
    expect(Math.abs(content.y + content.height / 2 - rect.y - rect.height / 2)).toBeLessThan(1)
    for (const item of [cell.getByRole('button'), cell.locator(':scope > div > span')]) {
      const bounds = (await item.boundingBox())!
      expect(Math.abs(bounds.x - content.x)).toBeLessThan(1)
    }
    const actions = row.locator('td').last()
    const actionBounds = (await actions.boundingBox())!
    const middleButton = (await actions.getByRole('button').nth(1).boundingBox())!
    expect(Math.abs(middleButton.x + middleButton.width / 2 - headerBounds.x - headerBounds.width / 2)).toBeLessThan(1)
    for (const button of await actions.getByRole('button').all()) {
      const bounds = (await button.boundingBox())!
      expect(bounds.x).toBeGreaterThanOrEqual(actionBounds.x)
      expect(bounds.x + bounds.width).toBeLessThanOrEqual(actionBounds.x + actionBounds.width)
      expect(Math.abs(bounds.y + bounds.height / 2 - actionBounds.y - actionBounds.height / 2)).toBeLessThan(1)
    }
  }
  const viewport = page.locator('[data-slot="table-viewport"]')
  if (width >= 1440) {
    expect(await viewport.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true)
    await expect(rows.first().getByRole('button', { name: '删除', exact: true })).toBeInViewport({ ratio: 1 })
  } else {
    expect(await viewport.evaluate((el) => el.scrollWidth > el.clientWidth)).toBe(true)
  }
  await expect(rows.nth(1).locator('time')).toHaveAttribute('datetime', '2030-10-01T12:30:00Z')
  await expect(rows.nth(1).locator('time > span')).toHaveCount(2)
  const left = (await tokenCell.boundingBox())!.x
  await page.locator('[data-slot="table-viewport"]').evaluate((el) => { el.scrollLeft = el.scrollWidth })
  expect(Math.abs((await tokenCell.boundingBox())!.x - left)).toBeLessThan(1)
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  await page.locator('[data-slot="table-viewport"]').evaluate((el) => { el.scrollLeft = 0 })
  await page.screenshot({ path: `test-results/key-list-layout-${width}.png`, animations: 'disabled' })
})

for (const { width, language } of [{ width: 1440, language: 'zh-CN' }, { width: 1440, language: 'en' }, { width: 1920, language: 'zh-CN' }]) {
  test(`密钥整表视觉 ${width} ${language}：列头基线、信息层次与悬停一致`, async ({ page }) => {
    await prepare(page, 'account', language)
    await page.setViewportSize({ width, height: 1000 })
    await page.route('**/api/me/keys?*', (route) => route.fulfill({ json: {
      total: 14, data: Array.from({ length: 14 }, (_, index) => ({
        ...keys[index % 2], id: index + 42,
        name: ['production', '客服助手', 'web', 'development'][index % 4],
        key_prefix: `sk-okapi-fixture${index}`, status: index === 2 ? 2 : 1,
        usage_trend: index < 3 ? keys[index % 2].usage_trend : undefined,
        used_micro: index === 1 ? 8634700 : index === 2 ? 0 : 1100,
        quota_mode: index === 1 ? 1 : 0, quota_micro: index === 1 ? 50000000 : null,
        requests: index === 1 ? 1820 : index === 2 ? 0 : 18,
        rpm_limit: index === 1 ? 600 : null,
        expires_at: index === 1 ? '2030-10-01T12:30:00Z' : index === 2 ? '2020-10-01T12:30:00Z' : null,
      })),
    } }))
    await page.goto('/portal/keys')
    const table = page.getByRole('table')
    await expect(table.locator('tbody tr')).toHaveCount(14)
    await expect(table.locator('thead th')).toHaveText(language === 'en'
      ? ['Token', 'Name', 'Status', 'Expires', 'Spend / Limit', 'Requests', 'RPM limit', 'Created', 'Tokens · 7 days', 'Actions']
      : ['Token', '名称', '状态', '有效期', '消费 / 额度', '累计请求', 'RPM 上限', '创建时间', '近 7 天 Token', '操作'])
    expect(await page.locator('[data-slot="table-viewport"]').evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true)
    for (const heading of await table.locator('thead th').all()) {
      expect(await heading.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true)
    }
    for (const row of await table.locator('tbody tr').all()) expect((await row.boundingBox())!.height).toBe(56)
    const first = table.locator('tbody tr').first()
    const tokenCell = first.locator('td').first()
    const before = await tokenCell.evaluate((el) => getComputedStyle(el).backgroundColor)
    await first.locator('td').nth(1).hover()
    await expect.poll(() => tokenCell.evaluate((el) => getComputedStyle(el).backgroundColor)).not.toBe(before)
    const name = first.locator('td').nth(1)
    const spend = first.locator('td').nth(4)
    const namePrimary = (await name.getByRole('button').boundingBox())!
    const spendPrimary = (await spend.locator(':scope > span').first().boundingBox())!
    const nameSecondary = (await name.locator(':scope > div > span').boundingBox())!
    const spendSecondary = (await spend.locator(':scope > span').last().boundingBox())!
    expect(Math.abs(namePrimary.y - spendPrimary.y)).toBeLessThan(1)
    expect(Math.abs(nameSecondary.y - spendSecondary.y)).toBeLessThan(1)
    await expect(name.locator(':scope > div > span')).toHaveCSS('font-size', '11px')
    await expect(spend.locator(':scope > span').last()).toHaveCSS('font-size', '11px')
    await page.locator('#main-content').getByRole('heading', { name: language === 'en' ? 'Keys' : '密钥', exact: true }).hover()
    await expect(page.locator('[data-slot="key-usage-sparkline"]').nth(3)).toHaveText(language === 'en' ? 'Upgrade needed' : '待升级')
    await page.screenshot({ path: `test-results/key-list-polished-${width}-${language}.png`, animations: 'disabled' })
  })
}

test('密钥趋势：旧后端与加载失败有明确状态，刷新后显示真实汇总', async ({ page }) => {
  const queries = await prepare(page)
  let updated = false
  await page.route('**/api/me/keys?*', (route) => route.fulfill({ json: {
    total: 3,
    data: updated ? [keys[0], keys[1], { ...keys[0], id: 44, usage_trend: null }] : [
      { ...keys[0], usage_trend: undefined }, keys[1], { ...keys[0], id: 44, usage_trend: null },
    ],
  } }))
  await page.goto('/portal/keys')
  await expect(page.getByText('当前后端未返回密钥趋势数据', { exact: false })).toBeVisible()
  const charts = page.locator('[data-slot="key-usage-sparkline"]')
  await expect(charts.nth(0)).toHaveText('待升级')
  await expect(charts.nth(0).locator('svg')).toHaveCount(0)
  await expect(charts.nth(1)).toContainText('暂无用量')
  await expect(charts.nth(1).locator('[data-slot="key-trend-total"]')).toHaveText('0')
  await expect(charts.nth(2)).toHaveText('暂不可用')
  updated = true
  await page.getByRole('button', { name: '刷新', exact: true }).click()
  await expect(charts.nth(0)).toHaveAttribute('data-state', 'ready')
  await expect(charts.nth(0).locator('[data-slot="key-trend-total"]')).toHaveText('2,390')
  await expect(page.getByText('当前后端未返回密钥趋势数据', { exact: false })).toHaveCount(0)
  expect(queries.filter((q) => q.pathname === '/api/me/logs/stat')).toHaveLength(0)
})

test('统计失败可重试或进入明细；真实缓存零与无首字样本分别显示', async ({ page }) => {
  await prepare(page)
  let failures = true, count = 0
  await page.route('**/api/me/logs/stat?*', (route) => {
    count++
    return route.fulfill(failures ? { status: 500, json: { error: { code: 'internal_error' } } } : {
      json: { ...stats, cached_tokens: 0, cache_read_samples: 77, avg_ttft_ms: null, ttft_samples: 0 },
    })
  })
  await page.goto('/portal/keys')
  await page.getByRole('button', { name: '查看 production（#42）用量统计' }).click()
  const dialog = page.getByRole('dialog', { name: '密钥用量概览' })
  await expect(dialog).toContainText('汇总暂不可用，仍可查看明细')
  await expect(dialog.getByRole('button', { name: /^实际消费/ })).toContainText('—')
  await expect(dialog.getByRole('button', { name: '查看 Token 明细' })).toBeEnabled()
  failures = false
  await dialog.getByRole('button', { name: '重试' }).click()
  await expect(dialog.getByRole('button', { name: /^缓存读取/ })).toContainText('缓存读取0')
  await expect(dialog).toContainText('暂无有效流式首字样本')
  await dialog.getByRole('button', { name: '刷新' }).click()
  await expect.poll(() => count).toBe(3)
  await expect(dialog).not.toContainText('汇总暂不可用')
})

for (const { width, language } of [{ width: 1920, language: 'zh-CN' }, { width: 1440, language: 'en' }, { width: 390, language: 'zh-CN' }]) {
  test(`密钥摘要布局 ${width}px ${language}：六项指标两列对齐，操作与焦点始终可达`, async ({ page }) => {
    await prepare(page, 'account', language)
    await page.setViewportSize({ width, height: 900 })
    await page.goto('/portal/keys')
    const english = language === 'en'
    await page.getByRole('button', { name: english ? 'View usage for production (#42)' : '查看 production（#42）用量统计', exact: true }).click()
    const dialog = page.getByRole('dialog', { name: english ? 'Key usage overview' : '密钥用量概览' })
    const cards = dialog.getByRole('region', { name: english ? 'Filtered summary' : '筛选范围汇总' }).getByRole('button')
    await expect(cards).toHaveCount(6)
    const boxes = await Promise.all((await cards.all()).map((card) => card.boundingBox()))
    for (let i = 0; i < 6; i += 2) {
      expect(Math.abs(boxes[i]!.y - boxes[i + 1]!.y)).toBeLessThanOrEqual(1)
      expect(Math.abs(boxes[i]!.width - boxes[i + 1]!.width)).toBeLessThanOrEqual(1)
      if (i > 0) expect(boxes[i]!.x).toBeCloseTo(boxes[0]!.x, 1)
    }
    expect(await dialog.evaluate((node) => node.scrollWidth <= node.clientWidth)).toBe(true)
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    const details = dialog.getByRole('button', { name: english ? 'View token details' : '查看 Token 明细', exact: true })
    await expect(details).toBeInViewport()
    await details.focus()
    await page.keyboard.press('Tab')
    await expect(dialog.locator('header').getByRole('button')).toBeFocused()
    await page.screenshot({ path: `test-results/key-usage-${width}-${language}.png`, animations: 'disabled' })
  })
}
