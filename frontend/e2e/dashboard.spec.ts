import { expect, test } from '@playwright/test'
import type { Page } from '@playwright/test'
import { fileURLToPath } from 'node:url'
import { readFile } from 'node:fs/promises'

const health = { postgres: true, redis: true, clickhouse: true, nats_connected: true, outbox_pending: 0, dlq_depth: 0, cooling_keys: 0, pricebook_epoch: 4 }
// 首页趋势分两路：图表走精简查询（fields=core，毫秒级），质量与 Token 构成共用一次完整查询（不查上期）。
const isChart = (url: string) => url.startsWith('/admin/stats/trend') && url.includes('fields=core')
const isUsage = (url: string) => url.startsWith('/admin/stats/trend') && !url.includes('fields=core')
const bucket = (requests: number) => ({ requests, tokens: requests * 1000, amount_micro: requests * 20000, errors: 42, error_rate_bp: 100, active_users: 28 })

async function showDistribution(page: Page, view: 'model' | 'channel' | 'tokens') {
  const tab = page.locator('[data-slot="dashboard-distribution"]').getByRole('tab').nth(['model', 'channel', 'tokens'].indexOf(view))
  await tab.click()
  await expect(tab).toHaveAttribute('aria-selected', 'true')
}

for (const { width, height, theme, language } of [
  { width: 1366, height: 768, theme: 'light', language: 'zh-CN' },
  { width: 1920, height: 1080, theme: 'light', language: 'zh-CN' },
  { width: 1366, height: 900, theme: 'dark', language: 'en' },
]) {
  test(`首页视觉 ${width}×${height} ${theme}：卡片标题对齐，环图自适应且图例完整`, async ({ page }) => {
    await prepare(page, language)
    await page.setViewportSize({ width, height })
    await page.addInitScript((theme) => localStorage.setItem('okapi.theme', theme), theme)
    await page.emulateMedia({ reducedMotion: 'reduce' })
    await page.goto('/admin')
    const trend = page.locator('[data-slot="dashboard-trend"]')
    const distribution = page.locator('[data-slot="dashboard-distribution"]')
    await expect(trend.getByLabel(language === 'en' ? 'Trend plot' : '趋势绘图区')).toBeVisible()
    await page.evaluate(() => document.fonts.ready)
    const [leftTitle, rightTitle] = await Promise.all([trend, distribution].map((card) => card.locator('h3').first().boundingBox()))
    expect(Math.abs(leftTitle!.y + leftTitle!.height / 2 - rightTitle!.y - rightTitle!.height / 2)).toBeLessThanOrEqual(2)
    const charts = page.locator('[data-slot="dashboard-charts"]')
    expect(await charts.evaluate((node) => Number.parseFloat(getComputedStyle(node).columnGap))).toBe(height >= 900 ? 12 : 8)
    for (const view of ['model', 'channel', 'tokens'] as const) {
      await showDistribution(page, view)
      expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth && document.documentElement.scrollHeight <= innerHeight)).toBe(true)
      if (view !== 'tokens') {
        const bars = distribution.locator('[data-slot="distribution-bar"]:visible')
        await expect(bars).toHaveCount(5)
        const card = (await distribution.boundingBox())!
        const tracks = await bars.evaluateAll((nodes) => nodes.map((node) => node.parentElement!.getBoundingClientRect()))
        expect(Math.max(...tracks.map((track) => track.x)) - Math.min(...tracks.map((track) => track.x))).toBeLessThanOrEqual(1)
        expect(Math.max(...tracks.map((track) => track.width)) - Math.min(...tracks.map((track) => track.width))).toBeLessThanOrEqual(1)
        for (const bar of await bars.all()) {
          const box = (await bar.boundingBox())!
          expect(box.height).toBe(6)
          expect(box.y + box.height).toBeLessThanOrEqual(card.y + card.height - 1)
        }
      }
      await page.screenshot({ path: test.info().outputPath(`dashboard-${view}.png`), animations: 'disabled' })
    }
    const donut = (await distribution.locator('[data-slot="token-donut"]').boundingBox())!
    const legend = (await distribution.locator('[data-slot="token-segments"]').boundingBox())!
    const card = (await distribution.boundingBox())!
    expect(donut.width).toBe(height >= 900 ? 176 : 96)
    expect(Math.abs(donut.width - donut.height)).toBeLessThanOrEqual(1)
    expect(Math.abs(donut.y + donut.height / 2 - legend.y - legend.height / 2)).toBeLessThanOrEqual(1)
    expect(legend.x + legend.width).toBeLessThanOrEqual(card.x + card.width - 1)
    expect(donut.y + donut.height).toBeLessThanOrEqual(card.y + card.height - 1)
  })
}

for (const width of [1024, 1366, 1920]) {
  test(`首页日期位置 ${width}px：日期时区在今日左侧，不再显示在标题下方`, async ({ page }) => {
    await prepare(page)
    await page.setViewportSize({ width, height: 900 })
    await page.route('**/admin/stats/overview?*', (route) => route.fulfill({ json: {
      days: 7, today: bucket(4200), yesterday: bucket(3600), window: bucket(8400),
      calendar: { start_date: '2026-09-20', end_date: '2026-09-26', today: '2026-09-26', timezone: 'America/Los_Angeles' },
    } }))
    await page.goto('/admin')
    const header = page.locator('[data-slot="page-header"]')
    const date = header.locator('[data-slot="dashboard-calendar"]')
    const today = header.getByRole('button', { name: '今日', exact: true })
    await expect(date).toHaveText('2026-09-20 — 2026-09-26 · America/Los_Angeles')
    const [dateBox, todayBox] = await Promise.all([date.boundingBox(), today.boundingBox()])
    expect(dateBox!.x + dateBox!.width).toBeLessThanOrEqual(todayBox!.x)
    expect(Math.abs(dateBox!.y + dateBox!.height / 2 - todayBox!.y - todayBox!.height / 2)).toBeLessThanOrEqual(1)
    await expect(header.locator('h1').locator('..').locator('..').getByText('2026-09-20 — 2026-09-26 · America/Los_Angeles')).toHaveCount(0)
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    await page.screenshot({ path: test.info().outputPath('dashboard-date-position.png'), animations: 'disabled' })
  })
}

for (const viewport of [{ width: 1024, height: 768 }, { width: 1366, height: 768 }, { width: 1920, height: 1080 }]) {
  test(`首页首屏 ${viewport.width}×${viewport.height}：图表铺满剩余高度，切换与展开不撑出屏幕`, async ({ page }) => {
    await prepare(page)
    await page.setViewportSize(viewport)
    await page.emulateMedia({ reducedMotion: 'reduce' })
    await page.goto('/admin')
    const workspace = page.locator('[data-slot="dashboard-workspace"]')
    const trend = page.locator('[data-slot="dashboard-trend"]')
    const distribution = page.locator('[data-slot="dashboard-distribution"]')
    const checkViewport = async () => {
      const [frame, left, right] = await Promise.all([workspace, trend, distribution].map((node) => node.boundingBox()))
      expect(Math.abs(left!.y - right!.y)).toBeLessThanOrEqual(1)
      expect(Math.abs(left!.y + left!.height - right!.y - right!.height)).toBeLessThanOrEqual(1)
      expect(Math.abs(left!.y + left!.height - frame!.y - frame!.height)).toBeLessThanOrEqual(1)
      expect(left!.y + left!.height).toBeLessThanOrEqual(viewport.height - 11)
      expect(await page.evaluate(() => document.documentElement.scrollHeight <= innerHeight && document.documentElement.scrollWidth <= innerWidth)).toBe(true)
      expect(await workspace.evaluate((node) => node.scrollHeight <= node.clientHeight)).toBe(true)
    }
    await expect(trend.getByLabel('趋势绘图区')).toBeVisible()
    const sizes: number[] = []
    for (const view of ['model', 'channel', 'tokens'] as const) {
      await showDistribution(page, view)
      await checkViewport()
      sizes.push((await distribution.boundingBox())!.height)
      const plot = (await trend.getByLabel('趋势绘图区').boundingBox())!
      const chart = (await trend.boundingBox())!
      expect(plot.y + plot.height).toBeLessThanOrEqual(chart.y + chart.height - 1)
      if (view === 'tokens') {
        const donut = (await distribution.locator('[data-slot="token-donut"]').boundingBox())!
        const card = (await distribution.boundingBox())!
        expect(donut.y + donut.height).toBeLessThanOrEqual(card.y + card.height - 1)
      } else {
        const bar = (await distribution.locator('[data-slot="distribution-bar"]:visible').last().boundingBox())!
        const card = (await distribution.boundingBox())!
        expect(bar.y + bar.height).toBeLessThanOrEqual(card.y + card.height - 1)
      }
    }
    expect(Math.max(...sizes) - Math.min(...sizes)).toBeLessThanOrEqual(1)
    await page.screenshot({ path: test.info().outputPath('dashboard-fit-viewport.png'), animations: 'disabled' })
    await distribution.locator('summary').click()
    await page.getByRole('button', { name: '费用明细', exact: true }).click()
    await checkViewport()
    // Extra provenance, cost details and chart tables scroll inside their allocated cards.
    await trend.getByRole('button', { name: '数据表', exact: true }).click()
    await checkViewport()
    await expect(trend.getByRole('table')).toBeVisible()
    await page.screenshot({ path: test.info().outputPath('dashboard-fit-expanded.png'), animations: 'disabled' })
    await page.getByRole('button', { name: '费用明细', exact: true }).click()
    await page.route('**/api/notice', (route) => route.fulfill({ json: { notice: { title: '维护通知', body: '本次通知与统计卡片同时显示，图表仍需保持在首屏。', level: 'info', updated_at: '2026-10-02T00:00:00Z' } } }))
    await page.reload()
    await expect(page.getByText('维护通知', { exact: true })).toBeVisible()
    await checkViewport()
  })
}

for (const { width, language } of [{ width: 1366, language: 'zh-CN' }, { width: 390, language: 'en' }]) {
  test(`用量分布 ${width}px ${language}：三签单图、键盘切换和返回保留视图，不重复请求或重复计数`, async ({ page }) => {
    await page.setViewportSize({ width, height: 900 })
    const requests = await prepare(page, language)
    await page.goto('/admin?days=30')
    const card = page.locator('[data-slot="dashboard-distribution"]')
    const tabs = card.getByRole('tab')
    await expect(card).toHaveCount(1)
    await expect(tabs).toHaveCount(3)
    await expect(card.getByRole('tabpanel')).toHaveCount(1)
    await expect(card.locator('[data-slot="distribution-bars"]:visible')).toHaveCount(1)
    await expect(card.locator('[data-slot="distribution-bar"]:visible').first()).toHaveAttribute('data-share-bp', '2500')
    await expect(card.locator('[data-slot="distribution-bar"]:visible').first()).toHaveAttribute('style', /width: 25%/)
    await expect(card).toContainText(language === 'en' ? 'Top 5 share 75.0%' : '前 5 项占比 75.0%')
    await card.scrollIntoViewIfNeeded()
    const scroll = await page.evaluate(() => window.scrollY)
    await tabs.nth(0).focus()
    await tabs.nth(0).press('ArrowRight')
    await expect(tabs.nth(1)).toBeFocused()
    await expect(tabs.nth(0)).toHaveAttribute('aria-selected', 'true')
    await tabs.nth(1).press('Enter')
    await expect(page).toHaveURL('/admin?days=30&distribution=channel')
    await expect(tabs.nth(1)).toHaveAttribute('aria-selected', 'true')
    expect(Math.abs(await page.evaluate(() => window.scrollY) - scroll)).toBeLessThanOrEqual(1)
    await expect(card.getByRole('listitem')).toHaveCount(5)
    await expect(card.getByRole('listitem').first().getByRole('link')).toHaveAttribute('href', '/admin/stats?days=30&channel_id=1')
    await tabs.nth(1).press('End')
    await expect(tabs.nth(2)).toBeFocused()
    await tabs.nth(2).press('Space')
    await expect(page).toHaveURL('/admin?days=30&distribution=tokens')
    await expect(card.getByRole('tabpanel')).toHaveCount(1)
    await expect(card.locator('[data-slot="token-donut"]:visible')).toHaveCount(1)
    await expect(card.locator('[data-slot="distribution-bars"]:visible')).toHaveCount(0)
    await expect(card.locator('[data-slot="token-segments"] dd')).toHaveText(language === 'en' ? ['240K', '140K', '20,000', '170K', '30,000'] : ['24万', '14万', '20,000', '17万', '30,000'])
    const donut = card.locator('[data-slot="token-donut"]')
    await expect(donut).toHaveAttribute('style', /conic-gradient/)
    await expect(donut).toHaveAttribute('style', /100%/)
    expect(requests.filter(isUsage)).toHaveLength(1)
    expect(requests.filter((url) => url.startsWith('/admin/stats/breakdown?'))).toHaveLength(2)
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    await page.reload()
    await expect(tabs.nth(2)).toHaveAttribute('aria-selected', 'true')
    await showDistribution(page, 'channel')
    await page.goBack()
    await expect(tabs.nth(2)).toHaveAttribute('aria-selected', 'true')
    await page.goForward()
    await expect(tabs.nth(1)).toHaveAttribute('aria-selected', 'true')
    await card.screenshot({ path: test.info().outputPath('distribution-channels.png'), animations: 'disabled' })
    await showDistribution(page, 'tokens')
    await card.screenshot({ path: test.info().outputPath('distribution-tokens.png'), animations: 'disabled' })
    await showDistribution(page, 'model')
    await card.screenshot({ path: test.info().outputPath('distribution-models.png'), animations: 'disabled' })
    await page.goto('/admin?distribution=invalid')
    await expect(tabs.nth(0)).toHaveAttribute('aria-selected', 'true')
  })
}

async function prepare(page: Page, language = 'zh-CN') {
  const requests: string[] = []
  await page.addInitScript((language) => {
    localStorage.setItem('okapi.key', 'dashboard-fixture')
    localStorage.setItem('okapi.lang', language)
  }, language)
  await page.route('**/*', (route) => {
    const request = route.request()
    const url = new URL(request.url())
    if (request.isNavigationRequest()) return route.fulfill({ path: fileURLToPath(new URL('../dist/index.html', import.meta.url)), contentType: 'text/html' })
    if (!/^\/(api|admin|auth)\//.test(url.pathname)) return route.continue()
    expect(request.method()).toBe('GET')
    requests.push(url.pathname + url.search)
    const days = Number(url.searchParams.get('days') ?? 7)
    const endDate = url.searchParams.get('end_date') ?? '2026-09-26'
    const startDate = url.searchParams.get('start_date') ?? new Date(Date.parse(`${endDate}T00:00:00Z`) - (days - 1) * 86400_000).toISOString().slice(0, 10)
    const calendar = { start_date: startDate, end_date: endDate, today: '2026-09-26', timezone: 'UTC', generated_at: '2026-09-26 23:00:00' }
    const fixtures: Record<string, unknown> = {
      '/api/me': { user_id: 1, key_id: 1, role: 100, group: 'default', balance_micro: 200000000, permissions: ['*'] },
      '/api/notice': { notice: null },
      '/admin/diagnose': health,
      '/admin/reconciliation': { drift_count: 0, drifts: [] },
      '/admin/stats/overview': { days, calendar, today: bucket(4200), yesterday: bucket(3600), window: bucket(days === 1 ? 4200 : days === 30 ? 36000 : 8400) },
      '/admin/stats/trend': { days, granularity: 'day', window: {
        ...calendar, start_at: `${startDate} 00:00:00`, end_at: `${endDate} 23:59:59`,
      }, data: [
        { bucket: '2026-09-20', requests: 2800, amount_micro: 56000000, tokens: 200000 },
        { bucket: '2026-09-23', requests: 1400, amount_micro: 28000000, tokens: 100000 },
        { bucket: '2026-09-26', requests: 4200, amount_micro: 84000000, tokens: 300000 },
      ].filter((row) => row.bucket >= startDate && row.bucket <= endDate), total: { requests: 8400, errors: 84, avg_output_tps_milli: 42500, cost_known_requests: 4200, cost_coverage_bp: 5000, known_cost_micro: 4200000, known_margin_micro: 1800000, avg_latency_ms: 1300, avg_ttft_ms: 150, ttft_samples: 6000, prompt_tokens: 400000, cached_tokens: 140000, cache_read_known_requests: 8400, cache_write_known_requests: 8400, cache_write_tokens: 20000, completion_tokens: 200000, reasoning_tokens: 30000, cache_hit_bp: 3500 } },
      '/admin/stats/breakdown': { days, total_amount_micro: 10000000, total_requests: 10000, data: Array.from({ length: 5 }, (_, i) => ({
        rank: i + 1, key: url.searchParams.get('by') === 'channel' ? String(i + 1) : ['gpt-5.1', 'claude-sonnet-4.5', 'gemini-2.5-pro', 'deepseek-chat', 'qwen3-coder'][i],
        label: url.searchParams.get('by') === 'channel' ? ['OpenAI Primary', 'Anthropic Direct', 'Google Cloud', 'DeepSeek Official', 'Alibaba Cloud'][i] : null,
        channel_id: i + 1, amount_micro: (5 - i) * 500000, share_bp: (5 - i) * 500, requests: (5 - i) * 500,
      })) },
      '/admin/stats/margin': { days, window: { start_date: days === 30 ? '2026-08-28' : '2026-09-20', end_date: '2026-09-26', timezone: 'UTC' }, data: [
        { day: '2026-09-20', requests: 2800, amount_micro: 56000000 },
        { day: '2026-09-23', requests: 1400, amount_micro: 28000000 },
        { day: '2026-09-26', requests: 4200, amount_micro: 84000000 },
      ] },
      '/admin/stats/realtime': { window_secs: 60, qps_milli: 1800, requests: 108, errors: 1, error_rate_bp: 93, tokens: 148000, amount_micro: 2310000,
        series: Array.from({ length: 60 }, (_, i) => ({ ts: i, requests: (i * 17) % 8, errors: 0, tokens: 0, amount_micro: 0 })) },
      '/admin/stats/inventory': {
        users: { total: 1258, active: 1100, new_today: 16, new_7d: 56 }, api_keys: { total: 3210, active: 2856, used_7d: 1560 },
        channels: { total: 24, healthy: 22, no_key: 0, auto_disabled: 2 }, models: { total: 136, priced: 132, served: 128 },
      },
    }
    return route.fulfill({ json: fixtures[url.pathname] ?? { data: [] } })
  })
  return requests
}

test('首页完整趋势查询只发一次由质量与 Token 共用，图表与排行走精简查询，短期复用，手动刷新绕过后端缓存', async ({ page }) => {
  const requests = await prepare(page)
  const fresh: string[] = []
  page.on('request', (request) => {
    if (request.headers()['cache-control'] === 'no-cache') fresh.push(new URL(request.url()).pathname)
  })
  await page.goto('/admin')
  await expect(page.getByRole('region', { name: '经营概览' }).getByRole('link')).toHaveCount(5)
  await expect.poll(() => requests.filter(isUsage).length).toBe(1)
  expect(requests.filter(isChart)).toHaveLength(1)
  expect(requests.filter(isUsage)[0]).toContain('compare=false')
  for (const url of requests.filter((url) => url.startsWith('/admin/stats/trend?'))) expect(url).toContain('cached=true')
  // 排行只读金额 / 请求 / Token 与占比：精简查询，不查上期。
  const rankings = requests.filter((url) => url.startsWith('/admin/stats/breakdown?'))
  expect(rankings).toHaveLength(2)
  for (const url of rankings) for (const part of ['cached=true', 'fields=core', 'compare=false']) expect(url).toContain(part)
  await page.getByRole('button', { name: '刷新', exact: true }).click()
  for (const path of ['/admin/stats/overview', '/admin/stats/trend', '/admin/stats/breakdown']) {
    await expect.poll(() => fresh.includes(path)).toBe(true)
  }
  await expect.poll(() => requests.filter(isUsage).length).toBe(2)
  await expect.poll(() => requests.filter(isChart).length).toBe(2)
})

for (const width of [390, 768, 1366]) {
  test(`首页 ${width}px 指标按 Token、请求、错误率、收入、活跃用户排序，明细入口保持对应指标`, async ({ page }) => {
    await prepare(page)
    await page.setViewportSize({ width, height: 900 })
    await page.goto('/admin')
    const cards = page.getByRole('region', { name: '经营概览' }).getByRole('link')
    await expect(cards).toHaveCount(5)
    const metrics = [
      ['Token 数', 'measure=tokens'], ['请求数', 'measure=requests'],
      ['错误率', 'measure=error_rate'], ['收入', 'measure=amount'],
      ['活跃用户', 'view=breakdown&by=user'],
    ]
    for (const [index, [label, query]] of metrics.entries()) {
      await expect(cards.nth(index)).toHaveAccessibleName(new RegExp(`近 7 天 · ${label}`))
      await expect(cards.nth(index)).toHaveAttribute('href', `/admin/stats?days=7&${query}`)
    }
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  })
}

test('Dashboard 指标与详情默认使用所选日期，键盘进入分析及返回保留同一范围', async ({ page }) => {
  const requests = await prepare(page)
  await page.goto('/admin?days=30')
  const summary = page.getByRole('region', { name: '经营概览' })
  await expect(summary.getByRole('link')).toHaveCount(5)
  const expected = [
    ['请求数', 'measure=requests'], ['收入', 'measure=amount'], ['Token 数', 'measure=tokens'],
    ['活跃用户', 'view=breakdown&by=user'], ['错误率', 'measure=error_rate'],
  ]
  for (const [label, suffix] of expected) {
    await expect(summary.getByRole('link', { name: new RegExp(`近 30 天 · ${label}`) })).toHaveAttribute('href', `/admin/stats?days=30&${suffix}`)
  }
  const token = summary.getByRole('link', { name: /近 30 天 · Token 数/ })
  await token.focus()
  await expect(token).toBeFocused()
  await token.press('Enter')
  await expect(page).toHaveURL('/admin/stats?days=30&measure=tokens')
  await expect.poll(() => requests.some((url) => url === '/admin/stats/trend?days=30&metric=tokens')).toBe(true)
  await page.goBack()
  await expect(page.getByRole('button', { name: '近 30 天', exact: true })).toHaveAttribute('aria-pressed', 'true')
  for (const [label, suffix] of expected) {
    await expect(summary.getByRole('link', { name: new RegExp(`近 30 天 · ${label}`) })).toHaveAttribute('href', `/admin/stats?days=30&${suffix}`)
  }
  await summary.getByRole('link', { name: /近 30 天 · 活跃用户/ }).click()
  await expect(page).toHaveURL('/admin/stats?days=30&view=breakdown&by=user')
  await expect.poll(() => requests.some((url) => url === '/admin/stats/breakdown?days=30&by=user&limit=50')).toBe(true)
  await page.goBack()
  await expect(page).toHaveURL('/admin?days=30')
})

test('Dashboard 无请求时不把错误率显示为 0%，仍可进入同窗分析', async ({ page }) => {
  await prepare(page)
  await page.route('**/admin/stats/overview?*', (route) => route.fulfill({ json: { days: 1, today: bucket(0), yesterday: bucket(3600), window: bucket(0) } }))
  await page.goto('/admin?days=1')
  const error = page.getByRole('region', { name: '经营概览' }).getByRole('link', { name: /今日 · 错误率/ })
  await expect(error.getByTitle('—', { exact: true })).toBeVisible()
  await expect(error).not.toContainText('100%')
  await expect(error).toHaveAttribute('href', '/admin/stats?days=1&measure=error_rate')
})

for (const width of [320, 390]) {
  test(`Dashboard ${width}px 慢请求：骨架保持卡片列宽，加载中没有可误点的指标`, async ({ page }) => {
    await prepare(page)
    await page.setViewportSize({ width, height: 800 })
    let release: () => void = () => undefined
    const pending = new Promise<void>((resolve) => { release = resolve })
    await page.route('**/admin/stats/overview?*', async (route) => {
      await pending
      await route.fulfill({ json: { days: 7, today: bucket(4200), yesterday: bucket(3600), window: bucket(8400) } })
    })
    await page.goto('/admin')
    const summary = page.getByRole('region', { name: '经营概览' })
    try {
      await expect(summary.locator('[aria-busy=true]')).toBeVisible()
      await expect(summary.getByRole('link')).toHaveCount(0)
      expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
      expect(await summary.evaluate((node) => [...node.querySelectorAll('.skeleton-shimmer')].every((placeholder) => placeholder.getBoundingClientRect().right <= node.getBoundingClientRect().right))).toBe(true)
    } finally { release() }
    await expect(summary.getByRole('link')).toHaveCount(5)
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  })
}

test('首页异常进入同窗渠道健康，质量页日期和页签刷新、前进后退不丢失', async ({ page }) => {
  const requests = await prepare(page)
  await page.route('**/admin/stats/channels?*', (route) => {
    const url = new URL(route.request().url())
    requests.push(url.pathname + url.search)
    const row = { channel_id: 42, name: 'Primary', provider: 'openai', requests: 100, errors: 7, error_rate_bp: 700, ttft_p50_ms: 100, ttft_p95_ms: 200, ttft_p99_ms: 300, failovers: 1, sticky_rate_bp: 9500, tokens_per_1k_sec: 10000, amount_micro: 100000 }
    return route.fulfill({ json: { total_items: 2, data: url.searchParams.get('limit') === '10' ? [row, { ...row, channel_id: 43, name: 'Additional' }] : [row] } })
  })
  await page.goto('/admin?days=30&scope=window')
  const attention = page.locator('#dashboard-attention')
  const issue = attention.getByRole('link', { name: /渠道错误率超 5%/ })
  await expect(issue).toHaveAttribute('href', '/admin/quality?days=30&tab=channels')
  await issue.click()
  await expect(page.getByRole('tab', { name: '渠道健康' })).toHaveAttribute('aria-selected', 'true')
  await expect(page.getByRole('tabpanel', { name: '渠道健康' })).toBeVisible()
  await expect(page.getByRole('button', { name: '近 30 天' })).toHaveAttribute('aria-pressed', 'true')
  await expect.poll(() => requests.includes('/admin/stats/channels?days=30&limit=10&offset=0')).toBe(true)
  await expect(page.getByRole('link', { name: 'Additional', exact: true })).toBeVisible()
  await page.reload()
  await expect(page.getByRole('tab', { name: '渠道健康' })).toHaveAttribute('aria-selected', 'true')
  await expect(page.getByRole('button', { name: '近 30 天' })).toHaveAttribute('aria-pressed', 'true')
  await page.getByRole('tab', { name: '错误分布' }).click()
  await page.getByRole('button', { name: '近 1 天' }).click()
  await expect(page).toHaveURL('/admin/quality?days=1&tab=errors')
  await expect.poll(() => requests.includes('/admin/stats/errors?days=1&limit=10&offset=0')).toBe(true)
  await page.goBack()
  await expect(page).toHaveURL('/admin/quality?days=30&tab=errors')
  await page.goBack()
  await expect(page).toHaveURL('/admin/quality?days=30&tab=channels')
  await page.goForward()
  await expect(page.getByRole('tab', { name: '错误分布' })).toHaveAttribute('aria-selected', 'true')
  await page.goto('/admin/quality?days=-1&tab=unknown')
  await expect(page.getByRole('button', { name: '近 7 天' })).toHaveAttribute('aria-pressed', 'true')
  await expect(page.getByRole('tab', { name: '质量趋势' })).toHaveAttribute('aria-selected', 'true')
  await page.setViewportSize({ width: 320, height: 800 })
  // 图表和滚动页签通过 ResizeObserver 重新分配宽度，检查重排后的状态。
  await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  await page.getByRole('tab', { name: '错误分布' }).click()
  await expect(page.getByRole('tab', { name: '错误分布' })).toBeInViewport({ ratio: 1 })
  await page.screenshot({ path: 'test-results/quality-mobile-320.png', fullPage: true, animations: 'disabled' })
})

test('Dashboard 一套日期选择同步所有历史统计，默认近 7 天，刷新保留且显示日期时区', async ({ page }) => {
  const requests = await prepare(page)
  await page.goto('/admin')
  const summary = page.getByRole('region', { name: '经营概览' })
  await expect(summary.getByTitle('8,400', { exact: true })).toBeVisible()
  await expect(page.getByRole('button', { name: '所选时段', exact: true })).toHaveCount(0)
  await expect(page.locator('[data-slot=page-header]')).toContainText('2026-09-20 — 2026-09-26 · UTC')
  await page.getByRole('button', { name: '今日', exact: true }).click()
  await expect(summary).toContainText('今日 · 请求数')
  await expect(summary.getByTitle('4,200', { exact: true })).toBeVisible()
  await expect(summary).not.toContainText('100%')
  await expect(page.getByRole('region', { name: '费用与调用质量', exact: true })).toContainText('今日')
  await expect(page.getByRole('region', { name: '模型消费排行', exact: true })).toContainText('今日 · 前 5 项')
  await showDistribution(page, 'tokens')
  await expect(page.getByRole('region', { name: 'Token 用量构成', exact: true })).toContainText('今日')
  await expect.poll(() => requests.filter((url) => /\/admin\/stats\/(overview|trend|breakdown)\?/.test(url)).filter((url) => new URL(url, 'http://test').searchParams.get('days') === '1').length).toBe(5)
  await page.getByRole('button', { name: '近 30 天', exact: true }).click()
  await expect(summary.getByTitle('36,000', { exact: true })).toBeVisible()
  await expect(page.getByRole('link', { name: '详细分析', exact: true })).toHaveAttribute('href', '/admin/stats?days=30')
  await page.reload()
  await expect(page.getByRole('button', { name: '近 30 天', exact: true })).toHaveAttribute('aria-pressed', 'true')
  await expect(summary.getByTitle('36,000', { exact: true })).toBeVisible()
})

test('今日为零但历史有调用时，默认显示近 7 天用量；切到今日也不出现误导的下降百分比', async ({ page }) => {
  await prepare(page)
  await page.route('**/admin/stats/overview?*', (route) => {
    const days = Number(new URL(route.request().url()).searchParams.get('days'))
    return route.fulfill({ json: { days, today: bucket(0), yesterday: bucket(229), window: bucket(days === 1 ? 0 : 229) } })
  })
  await page.goto('/admin')
  const summary = page.getByRole('region', { name: '经营概览' })
  await expect(summary.getByRole('link', { name: /近 7 天 · 请求数/ }).getByTitle('229', { exact: true })).toBeVisible()
  await expect(summary.getByRole('link', { name: /近 7 天 · Token 数/ })).toContainText('23万')
  await page.getByRole('button', { name: '今日', exact: true }).click()
  await expect(summary.getByRole('link', { name: /今日 · 请求数/ }).getByTitle('0', { exact: true })).toBeVisible()
  await expect(summary).toContainText('昨日全天 229')
  await expect(summary).not.toContainText('100%')
  await expect(summary.getByRole('link', { name: /今日 · Token 数/ })).toHaveAttribute('href', '/admin/stats?days=1&measure=tokens')
})

test('自定义日期同步概览、质量、图表和排行；详情、刷新、前进后退和同长度不同日期不串缓存', async ({ page }) => {
  const requests = await prepare(page)
  await page.goto('/admin')
  await page.getByText('自定义日期', { exact: true }).click()
  await page.getByLabel('开始日期', { exact: true }).fill('2026-09-10')
  await page.getByLabel('结束日期', { exact: true }).fill('2026-09-13')
  await page.getByRole('button', { name: '应用日期', exact: true }).click()
  await expect(page).toHaveURL('/admin?start_date=2026-09-10&end_date=2026-09-13')
  const summary = page.getByRole('region', { name: '经营概览' })
  await expect(summary).toContainText('自定义日期 · 请求数')
  await expect(page.locator('[data-slot=page-header]')).toContainText('2026-09-10 — 2026-09-13 · UTC')
  const selected = () => requests.filter((url) => /\/admin\/stats\/(overview|trend|breakdown)\?/.test(url)).filter((url) => new URL(url, 'http://test').searchParams.get('start_date') === '2026-09-10')
  await expect.poll(() => selected().length).toBe(5)
  for (const request of selected()) {
    const query = new URL(request, 'http://test').searchParams
    expect(query.get('days')).toBe('4')
    expect(query.get('end_date')).toBe('2026-09-13')
  }
  const dates = 'days=4&start_date=2026-09-10&end_date=2026-09-13'
  await expect(page.getByRole('link', { name: '详细分析', exact: true })).toHaveAttribute('href', `/admin/stats?${dates}`)
  await expect(summary.getByRole('link', { name: /自定义日期 · Token 数/ })).toHaveAttribute('href', `/admin/stats?${dates}&measure=tokens`)
  await expect(page.getByRole('link', { name: '查看质量趋势', exact: true })).toHaveAttribute('href', `/admin/stats?${dates}&measure=latency`)
  for (const by of ['model', 'channel'] as const) {
    await showDistribution(page, by)
    await expect(page.getByRole('region', { name: by === 'model' ? '模型消费排行' : '渠道消费排行', exact: true }).getByRole('link', { name: '查看全部', exact: true })).toHaveAttribute('href', `/admin/stats?${dates}&view=breakdown&by=${by}`)
  }
  await showDistribution(page, 'model')
  await expect(page.getByRole('region', { name: '实时流量', exact: true })).toContainText('最近 60 秒')
  await page.getByRole('button', { name: '刷新', exact: true }).click()
  await expect.poll(() => selected().length).toBe(10)
  await page.reload()
  await expect(summary).toContainText('自定义日期 · 请求数')
  await page.locator('details > summary').filter({ hasText: '2026-09-10 — 2026-09-13' }).click()
  await page.getByLabel('开始日期', { exact: true }).fill('2026-09-14')
  await page.getByLabel('结束日期', { exact: true }).fill('2026-09-17')
  await page.getByRole('button', { name: '应用日期', exact: true }).click()
  await expect.poll(() => requests.some((url) => url.startsWith('/admin/stats/overview?days=4&start_date=2026-09-14&end_date=2026-09-17'))).toBe(true)
  await page.goBack()
  await expect(page.locator('[data-slot=page-header]')).toContainText('2026-09-10 — 2026-09-13 · UTC')
  await page.goForward()
  await expect(page.locator('[data-slot=page-header]')).toContainText('2026-09-14 — 2026-09-17 · UTC')
  await page.getByRole('button', { name: '近 7 天', exact: true }).click()
  await expect(page).toHaveURL('/admin?days=7')
  await expect(summary).toContainText('近 7 天 · 请求数')
})

test('后端未确认自定义日期时不把默认窗口伪装成所选日期，概览失败也不显示旧数字', async ({ page }) => {
  await prepare(page)
  await page.route('**/admin/stats/overview?*', (route) => route.fulfill({ json: { days: 7, today: bucket(4200), yesterday: bucket(3600), window: bucket(8400) } }))
  await page.goto('/admin?start_date=2026-09-10&end_date=2026-09-13')
  const summary = page.getByRole('region', { name: '经营概览' })
  await expect(summary.getByRole('alert')).toContainText('后端未确认所选日期范围')
  await expect(summary.getByRole('link')).toHaveCount(0)
  await expect(summary).not.toContainText('8,400')
})

test('旧 scope 深链不再切换独立口径，非法或不完整的自定义日期回退到近 7 天', async ({ page }) => {
  const requests = await prepare(page)
  for (const search of ['scope=today', 'scope=window', 'start_date=2026-02-30&end_date=2026-03-01', 'start_date=2026-09-10', 'start_date=2025-01-01&end_date=2026-09-26']) {
    await page.goto(`/admin?${search}`)
    await expect(page.getByRole('region', { name: '经营概览' })).toContainText('近 7 天 · 请求数')
    await expect(page.getByRole('button', { name: '近 7 天', exact: true })).toHaveAttribute('aria-pressed', 'true')
  }
  expect(requests.filter((url) => url.startsWith('/admin/stats/overview?')).every((url) => !url.includes('start_date'))).toBe(true)
})

test('Dashboard：刷新覆盖当前卡片，图表补零且日均按完整时段计算，切换指标不重复查询', async ({ page }) => {
  const requests = await prepare(page)
  await page.goto('/admin')
  await page.getByRole('group', { name: '图表指标' }).getByRole('button', { name: '请求数', exact: true }).click()
  await expect(page.getByText('日均（含今日）').locator('..')).toContainText('1,200')
  await expect(page.getByText('单日峰值').locator('..')).toContainText('4,200 · 09-26')
  const trend = page.getByRole('group', { name: '请求量与收入趋势', exact: true })
  await trend.getByRole('button', { name: '数据表', exact: true }).click()
  await expect(trend.locator('tbody tr')).toHaveCount(7)
  await expect(trend.locator('tbody tr').filter({ hasText: '2026-09-21' })).toContainText('0')
  const before = requests.filter((r) => r.startsWith('/admin/stats/trend')).length
  await page.getByRole('group', { name: '图表指标' }).getByRole('button', { name: '实际消费' }).click()
  await expect(page.locator('[data-slot="dashboard-trend"]').getByText('时段累计', { exact: true }).locator('..')).toContainText('US$168.00')
  expect(requests.filter((r) => r.startsWith('/admin/stats/trend'))).toHaveLength(before)
  const healthCount = requests.filter((r) => r === '/admin/diagnose').length
  await page.getByRole('button', { name: '刷新', exact: true }).click()
  await expect.poll(() => requests.filter((r) => r === '/admin/diagnose').length).toBe(healthCount + 1)
  // 刷新同时覆盖精简图表与完整汇总两路。
  await expect.poll(() => requests.filter((r) => r.startsWith('/admin/stats/trend')).length).toBe(before + 2)
})

test('Dashboard 综合趋势默认同屏展示数量与收入，单位独立且导出保留两项原始值', async ({ page }) => {
  const requests = await prepare(page)
  await page.goto('/admin')
  const summary = page.getByRole('region', { name: '经营概览' })
  await expect(summary).toContainText('近 7 天 · 请求数')
  const trend = page.getByRole('group', { name: '请求量与收入趋势', exact: true })
  await expect(page.getByRole('group', { name: '图表指标' }).getByRole('button', { name: '综合' })).toHaveAttribute('aria-pressed', 'true')
  await expect(trend).toContainText('左轴：请求数 · 右轴：USD')
  await expect(page.getByText('每次请求均价').locator('..')).toContainText('US$0.02')
  await expect(trend.locator('.recharts-area')).toHaveCount(1)
  await expect(trend.locator('.recharts-line')).toHaveCount(1)
  await trend.locator('.recharts-line-dots circle').last().hover()
  const tooltip = trend.locator('.recharts-tooltip-wrapper')
  await expect(tooltip).toContainText('2026-09-26')
  await expect(tooltip).toContainText('4,200')
  await expect(tooltip).toContainText('US$84.00')
  await expect(tooltip).not.toContainText('当前显示合计')
  const legend = trend.getByRole('group', { name: '显示的序列' })
  const income = legend.getByRole('button', { name: '收入 (USD)右轴', exact: true })
  await income.click()
  await expect(income).toHaveAttribute('aria-pressed', 'false')
  await expect(legend.getByRole('button', { name: '请求数左轴', exact: true })).toBeDisabled()
  await expect(trend.locator('.recharts-line')).toHaveCount(0)
  await income.click()
  await trend.getByRole('button', { name: '数据表' }).click()
  await expect(trend.getByRole('columnheader')).toHaveText(['日期', '请求数', '收入 (USD)'])
  await expect(trend.locator('tbody tr').filter({ hasText: '2026-09-20' }).locator('td')).toHaveText(['2026-09-20', '2,800', 'US$56.00'])
  await expect(trend.locator('tbody tr').filter({ hasText: '2026-09-21' }).locator('td')).toHaveText(['2026-09-21', '0', 'US$0.00'])
  const download = page.waitForEvent('download')
  await trend.getByRole('button', { name: '导出 CSV' }).click()
  const csv = await readFile((await (await download).path())!, 'utf8')
  expect(csv).toContain('日期,请求数 (请求数),收入 (USD)')
  expect(csv).toContain('2026-09-20,2800,56')
  expect(requests.filter(isChart)).toHaveLength(1)
  expect(requests.filter(isUsage)).toHaveLength(1)
  await page.getByRole('button', { name: '近 30 天', exact: true }).click()
  await expect(summary).toContainText('近 30 天 · 请求数')
  await page.getByRole('button', { name: '近 7 天', exact: true }).click()
  await expect(summary).toContainText('近 7 天 · 请求数')
})

test('Dashboard：健康检查未完成与失败不能报全绿，实时与资源失败可重试', async ({ page }) => {
  await prepare(page)
  let release: () => void = () => undefined
  const pending = new Promise<void>((resolve) => { release = resolve })
  await page.route('**/admin/diagnose', async (route) => { await pending; await route.fulfill({ status: 500, json: { error: { code: 'internal_error' } } }) })
  for (const endpoint of ['realtime*', 'inventory']) await page.route(`**/admin/stats/${endpoint}`, (route) => route.fulfill({ status: 500, json: { error: { code: 'internal_error' } } }))
  await page.goto('/admin')
  await expect(page.getByRole('link', { name: '检查中', exact: true })).toBeVisible()
  await expect(page.locator('#dashboard-attention').getByRole('status')).toBeVisible()
  await expect(page.getByText('没有待办，一切正常。')).toHaveCount(0)
  release()
  await expect(page.getByRole('link', { name: '部分状态待确认', exact: true })).toBeVisible()
  await expect(page.getByText('部分检查未完成，暂时无法确认全部状态。')).toBeVisible()
  await expect(page.getByRole('region', { name: '实时流量' }).getByRole('alert').filter({ hasText: '实时数据暂不可用' })).toBeVisible()
  await expect(page.getByRole('region', { name: '站点速览', exact: true }).getByRole('alert')).toBeVisible()
  await expect(page.getByRole('heading', { name: '站点规模' })).toHaveCount(0)
  await expect(page.getByText('没有待办，一切正常。')).toHaveCount(0)
  await page.route('**/admin/diagnose', (route) => route.fulfill({ json: health }))
  await page.route('**/admin/stats/inventory', (route) => route.fulfill({ json: {
    users: { total: 1258, active: 1100, new_today: 16, new_7d: 56 }, api_keys: { total: 3210, active: 2856, used_7d: 1560 },
    channels: { total: 24, healthy: 24, no_key: 0, auto_disabled: 0 }, models: { total: 136, priced: 136, served: 136 },
  } }))
  await page.getByRole('alert').filter({ hasText: '部分检查未完成' }).getByRole('button', { name: '重试' }).click()
  await expect(page.getByText('没有待办，一切正常。')).toBeVisible()
  await expect(page.getByRole('link', { name: '暂无待办', exact: true })).toBeVisible()
})

test('Dashboard 待办摘要共用检查数据，可从首屏直接定位详情且保留日期和口径', async ({ page }) => {
  const requests = await prepare(page)
  await page.route('**/admin/diagnose', (route) => {
    requests.push('/admin/diagnose')
    return route.fulfill({ json: { ...health, redis: false, cooling_keys: 3 } })
  })
  await page.goto('/admin?days=30&scope=window')
  const status = page.getByRole('link', { name: '服务连接异常 · 4 项待处理', exact: true })
  await expect(status).toBeInViewport({ ratio: 1 })
  await expect(status).toContainText('服务连接异常')
  for (const endpoint of ['/admin/diagnose', '/admin/reconciliation', '/admin/models', '/admin/pools', '/admin/stats/channels?days=30']) {
    expect(requests.filter((url) => url === endpoint)).toHaveLength(1)
  }
  await status.focus()
  await expect(page.getByRole('tooltip')).toContainText('Redis')
  await expect(page.getByRole('tooltip')).toContainText('3 把渠道 key')
  await status.press('Escape')
  await expect(page.getByRole('tooltip')).toHaveCount(0)
  await status.press('Enter')
  await expect(page).toHaveURL('/admin?days=30&scope=window#dashboard-attention')
  await expect(page.locator('#dashboard-attention')).toBeInViewport()
  await expect(page.locator('#dashboard-attention')).toBeFocused()
  await expect(page.locator('#dashboard-attention')).toContainText('Redis')
  expect(requests.filter((url) => url.startsWith('/admin/stats/overview'))).toHaveLength(1)
})

test('Dashboard：成本覆盖率与质量同窗，未知成本不推算利润，刷新失败隐藏旧值', async ({ page }) => {
  const queries = await prepare(page)
  await page.goto('/admin')
  const summary = page.getByRole('region', { name: '费用与调用质量', exact: true })
  const costDetails = summary.getByRole('button', { name: '费用明细', exact: true })
  await expect(costDetails).toHaveAttribute('aria-expanded', 'false')
  await expect(summary.locator('dd')).toHaveCount(5)
  await expect(summary).not.toContainText('US$4.20')
  await costDetails.click()
  await expect(costDetails).toHaveAttribute('aria-expanded', 'true')
  await expect(summary).toContainText('US$4.20')
  await expect(summary).toContainText('US$1.80')
  await expect(summary).toContainText('50.0%')
  await summary.getByRole('button', { name: '部分成本未采集' }).focus()
  await expect(page.getByRole('tooltip')).toContainText('未采集部分不按零成本计算')
  await page.keyboard.press('Escape')
  await page.getByRole('button', { name: '近 30 天', exact: true }).click()
  await expect.poll(() => queries.filter(isUsage).at(-1)).toBe('/admin/stats/trend?days=30&metric=amount&cached=true&compare=false')
  await expect.poll(() => queries.filter(isChart).at(-1)).toBe('/admin/stats/trend?days=30&metric=amount&cached=true&fields=core')
  await expect(summary.getByRole('link', { name: '查看质量趋势' })).toHaveAttribute('href', '/admin/stats?days=30&measure=latency')
  await page.route('**/admin/stats/trend?*', (route) => route.fulfill({ status: 500, json: { error: { code: 'internal_error' } } }))
  await page.getByRole('button', { name: '刷新', exact: true }).click()
  await expect(summary.getByRole('alert')).toBeVisible()
  await expect(summary).not.toContainText('US$4.20')
  await page.route('**/admin/stats/trend?*', (route) => route.fulfill({ json: { data: [], total: { requests: 100, cost_known_requests: 0, cost_coverage_bp: 0, known_cost_micro: 0, known_margin_micro: null, avg_ttft_ms: 0, ttft_samples: 0, prompt_tokens: 0, cache_hit_bp: 0 } } }))
  await summary.getByRole('button', { name: '重试' }).click()
  await expect(summary.locator('dd').filter({ hasText: '—' })).toHaveCount(7)
  await expect(summary).not.toContainText('US$0.00')
})

test('Dashboard 质量摘要前移，各指标可键盘进入对应分析并保留时间窗', async ({ page }) => {
  const requests = await prepare(page)
  await page.goto('/admin?days=30')
  const operations = page.getByRole('region', { name: '费用与调用质量', exact: true })
  const trend = page.getByRole('group', { name: '请求量与收入趋势', exact: true })
  await expect(operations.locator('dd')).toHaveCount(5)
  expect((await operations.boundingBox())!.y).toBeLessThan((await trend.boundingBox())!.y)
  for (const [label, measure] of [['成功率', 'error_rate'], ['平均时延', 'latency'], ['首字延迟（TTFT）', 'ttft'], ['输出吞吐量', 'throughput'], ['缓存命中', 'cache']]) {
    const link = operations.getByRole('link', { name: new RegExp(`^${label} .*查看明细$`) })
    await expect(link).toHaveAttribute('href', `/admin/stats?days=30&measure=${measure}`)
    await link.focus()
    await expect(link).toBeFocused()
  }
  await operations.getByRole('link', { name: /^首字延迟（TTFT） .*查看明细$/ }).press('Enter')
  await expect(page).toHaveURL('/admin/stats?days=30&measure=ttft')
  await expect.poll(() => requests.includes('/admin/stats/trend?days=30&metric=ttft')).toBe(true)
  await page.goBack()
  await expect(page).toHaveURL('/admin?days=30')
  await expect(operations.locator('dd')).toHaveText(['99.0%', '1,300 ms', '150 ms', '42.5 Token/s', '35.0%'])
})

for (const { width, language, dark } of [
  { width: 1366, language: 'zh-CN', dark: false },
  { width: 1024, language: 'en', dark: false },
  { width: 1366, language: 'zh-CN', dark: true },
]) test(`Dashboard 费用默认收起，质量指标齐行，键盘展开不重复查询 ${width}px ${language} ${dark ? 'dark' : 'light'}`, async ({ page }) => {
  await page.setViewportSize({ width, height: 768 })
  const requests = await prepare(page, language)
  await page.goto('/admin')
  if (dark) await page.evaluate(() => document.documentElement.classList.add('dark'))
  const operations = page.getByRole('region', { name: language === 'en' ? 'Cost & call quality' : '费用与调用质量', exact: true })
  const toggle = operations.getByRole('button', { name: language === 'en' ? 'Cost details' : '费用明细', exact: true })
  const costs = page.locator(`[id="${await toggle.getAttribute('aria-controls')}"]`)
  await expect(toggle).toHaveAttribute('aria-expanded', 'false')
  await expect(costs).toBeHidden()
  await expect(operations.locator('dd')).toHaveText(['99.0%', '1,300 ms', '150 ms', '42.5 Token/s', '35.0%'])
  const tops = await operations.locator('dt').evaluateAll((nodes) => nodes.map((node) => node.getBoundingClientRect().top))
  expect(Math.max(...tops) - Math.min(...tops)).toBeLessThan(1)
  const collapsedHeight = (await operations.boundingBox())!.height
  await page.screenshot({ path: test.info().outputPath('quality-cost-collapsed.png'), animations: 'disabled' })
  await toggle.focus()
  await expect(toggle).toHaveAccessibleDescription(/Missing costs|未采集部分/)
  await toggle.press('Enter')
  await expect(toggle).toHaveAttribute('aria-expanded', 'true')
  await expect(costs).toBeVisible()
  const currency = language === 'en' ? '$' : 'US$'
  await expect(costs.locator('dd')).toHaveText([`${currency}4.20`, `${currency}1.80`, '50.0%'])
  await expect(operations.locator('dd')).toHaveText(['99.0%', '1,300 ms', '150 ms', '42.5 Token/s', '35.0%', `${currency}4.20`, `${currency}1.80`, '50.0%'])
  expect(requests.filter(isUsage)).toHaveLength(1)
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  await toggle.press('Escape')
  await page.mouse.move(0, 0)
  await operations.screenshot({ path: test.info().outputPath('quality-cost-expanded.png'), animations: 'disabled' })
  await toggle.press('Space')
  await expect(toggle).toHaveAttribute('aria-expanded', 'false')
  await expect(costs).toBeHidden()
  await expect(operations.locator('dd')).toHaveCount(5)
  expect(Math.abs((await operations.boundingBox())!.height - collapsedHeight)).toBeLessThan(1)
})

test('Dashboard 质量摘要展示已有吞吐量和缓存样本，完整指标优先，明确零值与缺失分开', async ({ page }) => {
  await prepare(page)
  let total: Record<string, number | null> = {
    requests: 1172, prompt_tokens: 4300000000,
    avg_output_tps_milli: null, observed_output_tps_milli: 205575, output_tps_samples: 752,
    cache_hit_bp: null, measured_cache_hit_bp: 3473, measured_cache_hit_requests: 54,
  }
  await page.route('**/admin/stats/trend?*', (route) => route.fulfill({ json: { data: [], total } }))
  await page.goto('/admin?days=7')
  const operations = page.getByRole('region', { name: '费用与调用质量', exact: true })
  const throughput = operations.getByRole('link', { name: /^输出吞吐量 / })
  const cache = operations.getByRole('link', { name: /^缓存命中 / })
  await expect(throughput).toHaveText('205.6 Token/s')
  await expect(cache).toHaveText('34.73%')
  await expect(throughput.locator('..').locator('..').locator('..')).toContainText('样本 752 / 1,172 次')
  await expect(cache.locator('..').locator('..').locator('..')).toContainText('样本 54 / 1,172 次')
  await expect(cache).toHaveAttribute('href', '/admin/stats?days=7&measure=cache')
  await throughput.focus()
  await expect(page.getByRole('tooltip')).toContainText('数值只代表这些样本')
  await page.keyboard.press('Escape')
  await cache.focus()
  await expect(page.getByRole('tooltip')).toContainText('缺失记录不按零命中计算')
  await page.keyboard.press('Escape')

  total = { ...total, observed_output_tps_milli: 0, measured_cache_hit_bp: 0 }
  await page.getByRole('button', { name: '刷新', exact: true }).click()
  await expect(throughput).toHaveText('0 Token/s')
  await expect(cache).toHaveText('0.0%')
  await expect(operations).toContainText('样本 54 / 1,172 次')

  total = { ...total, avg_output_tps_milli: 42500, cache_hit_bp: 3500 }
  await page.getByRole('button', { name: '刷新', exact: true }).click()
  await expect(throughput).toHaveText('42.5 Token/s')
  await expect(cache).toHaveText('35.0%')
  await expect(operations).not.toContainText('样本 54 / 1,172 次')
  await expect(operations).not.toContainText('样本 752 / 1,172 次')

  total = { requests: 1172, prompt_tokens: 4300000000 }
  await page.getByRole('button', { name: '刷新', exact: true }).click()
  await expect(throughput).toHaveText('—')
  await expect(cache).toHaveText('—')
  await expect(operations).not.toContainText('样本 54 / 1,172 次')
})

test('Dashboard 英文样本指标在窄屏保留样本范围，零请求不显示旧样本值', async ({ page }) => {
  await prepare(page, 'en')
  let requests = 100
  await page.route('**/admin/stats/trend?*', (route) => route.fulfill({ json: { data: [], total: {
    requests, avg_output_tps_milli: null, observed_output_tps_milli: 125500, output_tps_samples: 12,
    cache_hit_bp: null, measured_cache_hit_bp: 2000, measured_cache_hit_requests: 8,
  } } }))
  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto('/admin')
  const operations = page.getByRole('region', { name: 'Cost & call quality', exact: true })
  await expect(operations).toContainText('125.5 Token/s')
  await expect(operations).toContainText('20.0%')
  await expect(operations).toContainText('Samples 12 / 100 calls')
  await expect(operations).toContainText('Samples 8 / 100 calls')
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  requests = 0
  await page.getByRole('button', { name: 'Refresh', exact: true }).click()
  await expect(operations.getByRole('link', { name: /^Output throughput / })).toHaveText('—')
  await expect(operations.getByRole('link', { name: /^Cache hit / })).toHaveText('—')
  await expect(operations).not.toContainText('Samples')
})

for (const language of ['zh-CN', 'en']) test(`Dashboard 实时平均 QPS 与一分钟请求一致，低流量和微小收入保留精度 ${language}`, async ({ page }) => {
  await prepare(page, language)
  let calls = 10
  let failed = true
  await page.route('**/admin/stats/realtime?*', (route) => route.fulfill({ json: {
    window_secs: 60, qps_milli: 0, requests: calls, errors: failed ? calls : 0, error_rate_bp: failed ? 10000 : 0,
    tokens: failed ? 0 : 150, amount_micro: failed ? 0 : 25,
    // Traffic occurred earlier in the minute; the last ten seconds are idle.
    series: Array.from({ length: 60 }, (_, i) => ({ ts: i, requests: i < calls ? 1 : 0, errors: failed && i < calls ? 1 : 0, tokens: 0, amount_micro: 0 })),
  } }))
  await page.goto('/admin')
  const realtime = page.getByRole('region', { name: language === 'en' ? 'Live traffic' : '实时流量', exact: true })
  const qps = realtime.getByText(language === 'en' ? 'Avg QPS' : '平均 QPS', { exact: true }).locator('..')
  const tokens = realtime.getByText(language === 'en' ? 'Tokens' : 'Token 数', { exact: true }).locator('..')
  const income = realtime.getByText(language === 'en' ? 'Revenue' : '收入', { exact: true }).locator('..')
  const errorRate = realtime.getByText(language === 'en' ? 'Error rate' : '错误率', { exact: true }).locator('..')
  await expect(qps).toContainText('0.167')
  await expect(errorRate).toContainText('100.0%')
  await expect(tokens).toContainText('0')
  await expect(income).toContainText('0.00')
  await qps.locator('..').focus()
  await expect(page.getByRole('tooltip')).toContainText(language === 'en' ? 'Last 10 seconds QPS: 0' : '最近 10 秒 QPS：0')
  await expect(page.getByRole('tooltip')).toContainText(language === 'en' ? 'including failed requests' : '包含失败请求')
  await page.keyboard.press('Escape')
  const refresh = page.getByRole('button', { name: language === 'en' ? 'Refresh' : '刷新', exact: true })
  calls = 1
  await refresh.click()
  await expect(qps).toContainText('0.017')
  calls = 0
  await refresh.click()
  await expect(qps.locator('span').last()).toHaveText('0')
  await expect(errorRate).toContainText('—')
  calls = 3
  failed = false
  await refresh.click()
  await expect(qps).toContainText('0.05')
  await expect(tokens).toContainText('150')
  await expect(income).toContainText('0.000025')
  await expect(errorRate).toContainText('0%')
  await page.setViewportSize({ width: 390, height: 844 })
  await expect(qps).toBeVisible()
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
})

test('Dashboard 最近一分钟无调用不显示 0% 错误率，更新频率与数据时间可查', async ({ page }) => {
  await prepare(page)
  let calls = 0
  await page.route('**/admin/stats/realtime?*', (route) => route.fulfill({ json: {
    window_secs: 60, qps_milli: calls, requests: calls, errors: 0, error_rate_bp: 0, tokens: 0, amount_micro: 0, series: [],
  } }))
  await page.goto('/admin')
  const realtime = page.getByRole('region', { name: '实时流量', exact: true })
  const errorRate = realtime.getByText('错误率', { exact: true }).locator('..')
  await expect(errorRate).toContainText('—')
  await expect(errorRate).not.toContainText('0%')
  await expect(realtime).toContainText('最近 60 秒暂无请求')
  await realtime.getByText('实时', { exact: true }).locator('..').focus()
  await expect(page.getByRole('tooltip')).toContainText('最近 60 秒 · 每 5 秒更新')
  await expect(page.getByRole('tooltip')).toContainText('更新于')
  await page.keyboard.press('Escape')
  calls = 10
  await page.getByRole('button', { name: '刷新', exact: true }).click()
  await expect(errorRate).toContainText('0%')
  await expect(errorRate).not.toContainText('—')
})

test('Dashboard 同步缺口直接可见，恢复及刷新失败不沿用旧状态', async ({ page }) => {
  await prepare(page)
  let pending = 7
  let failed = 2
  await page.route('**/admin/stats/trend?*', (route) => route.fulfill({ json: {
    days: 7, data: [], total: { requests: 0, errors: 0, avg_output_tps_milli: 0 },
    window: { freshness: { stale: false, pending_events: pending, failed_events: failed, queue_age_seconds: 25, event_gap_seconds: null, checked_at: '2026-09-26T10:00:00Z', last_event_at: '2026-09-26T09:59:50Z', last_ingested_at: '2026-09-26T09:59:30Z' } },
  } }))
  await page.goto('/admin')
  const operations = page.getByRole('region', { name: '费用与调用质量', exact: true })
  // 请求为空时不能以 100% 成功或 0 Token/s 暗示采样正常。
  await expect(operations.getByRole('link', { name: /^成功率 / })).toContainText('—')
  await expect(operations.getByRole('link', { name: /^输出吞吐量 / })).toContainText('—')
  const delayed = operations.getByRole('button', { name: '统计尚未同步完整', exact: true })
  await expect(delayed).toBeInViewport({ ratio: 1 })
  await delayed.focus()
  await expect(page.getByRole('tooltip')).toContainText('待入库 7 · 失败 2 · 延迟 25 秒')
  await expect(delayed).toHaveAccessibleDescription(/汇总、排行与诊断每分钟更新/)
  await delayed.press('Escape')
  pending = 0
  failed = 0
  await page.getByRole('button', { name: '刷新', exact: true }).click()
  await expect(operations.getByRole('button', { name: /^统计更新于 / })).toBeVisible()
  await expect(delayed).toHaveCount(0)
  await page.route('**/admin/stats/trend?*', (route) => route.fulfill({ status: 500, json: { error: { code: 'internal_error' } } }))
  await page.getByRole('button', { name: '刷新', exact: true }).click()
  await expect(operations.getByRole('button', { name: '统计刷新失败', exact: true })).toBeVisible()
  await expect(operations.getByRole('button', { name: /^统计更新于 / })).toHaveCount(0)
})

test('Dashboard 汇总每分钟更新当前时段，后台和离开页面停止刷新', async ({ page }) => {
  await page.clock.install()
  const requests = await prepare(page)
  await page.goto('/admin')
  await expect(page.getByRole('region', { name: '费用与调用质量' }).locator('dd')).toHaveCount(5)
  const count = (prefix: string) => requests.filter((url) => url.startsWith(prefix)).length
  await page.clock.fastForward(60_000)
  await expect.poll(() => count('/admin/stats/overview?days=7')).toBe(2)
  expect(count('/admin/stats/margin')).toBe(0)
  await expect.poll(() => requests.filter(isUsage).length).toBe(2)
  await expect.poll(() => requests.filter(isChart).length).toBe(2)
  await expect.poll(() => count('/admin/stats/breakdown?days=7')).toBe(4)
  await page.getByRole('button', { name: '近 30 天', exact: true }).click()
  await expect(page.getByRole('region', { name: '费用与调用质量' })).toContainText('近 30 天')
  await expect.poll(() => count('/admin/stats/overview?days=30')).toBe(1)
  await page.clock.fastForward(60_000)
  await expect.poll(() => count('/admin/stats/overview?days=30')).toBe(2)
  expect(count('/admin/stats/overview?days=7')).toBe(2)
  await page.evaluate(() => Object.defineProperty(document, 'visibilityState', { configurable: true, value: 'hidden' }))
  await page.clock.fastForward(60_000)
  expect(count('/admin/stats/overview?days=30')).toBe(2)
  await page.evaluate(() => Object.defineProperty(document, 'visibilityState', { configurable: true, value: 'visible' }))
  await page.getByRole('link', { name: '详细分析', exact: true }).click()
  await expect(page).toHaveURL('/admin/stats?days=30')
  await expect(page.getByRole('region', { name: '费用与调用质量' })).toHaveCount(0)
  const afterLeaving = count('/admin/stats/overview?days=30')
  await page.clock.fastForward(60_000)
  expect(count('/admin/stats/overview?days=30')).toBe(afterLeaving)
})

for (const width of [1024, 1366]) {
  test(`Dashboard ${width}×768 首屏优先展示经营、质量和实时待办，趋势与分布完整可访问`, async ({ page }) => {
    await page.setViewportSize({ width, height: 768 })
    const requests = await prepare(page)
    await page.goto('/admin')
    for (const name of ['实时流量', '站点速览', '经营概览', '费用与调用质量', '优先处理']) {
      await expect(page.getByRole('region', { name, exact: true })).toBeInViewport({ ratio: 1 })
    }
    await expect(page.getByRole('group', { name: '请求量与收入趋势', exact: true })).toBeInViewport()
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    // 摘要布局共享原有汇总数据，不为每个小指标单独请求。
    expect(requests.filter(isChart)).toHaveLength(1)
    expect(requests.filter(isUsage)).toHaveLength(1)
    await page.screenshot({ path: `test-results/dashboard-firstscreen-${width}.png`, animations: 'disabled' })
    // 首屏卡片高度受视口约束，额外详情仍可在卡片内滚动访问。
    for (const name of ['模型消费排行', '渠道消费排行', 'Token 用量构成']) {
      await showDistribution(page, name === '模型消费排行' ? 'model' : name === '渠道消费排行' ? 'channel' : 'tokens')
      const panel = page.getByRole('region', { name, exact: true })
      await panel.scrollIntoViewIfNeeded()
      await expect(panel).toBeInViewport({ ratio: 1 })
    }
  })
}

test('首页侧栏随可用宽度自适应，手动展开与收起跨刷新和窗口变化保留', async ({ page }) => {
  await prepare(page)
  await page.setViewportSize({ width: 1366, height: 768 })
  await page.goto('/admin')
  await expect(page.getByRole('button', { name: '收起侧栏', exact: true })).toBeVisible()
  await page.setViewportSize({ width: 1024, height: 768 })
  await expect(page.getByRole('button', { name: '展开侧栏', exact: true })).toBeVisible()
  expect(await page.evaluate(() => localStorage.getItem('okapi.sidebar'))).toBeNull()
  // 自适应不写入偏好，扩大窗口可恢复完整导航。
  await page.setViewportSize({ width: 1366, height: 768 })
  await expect(page.getByRole('button', { name: '收起侧栏', exact: true })).toBeVisible()
  await page.setViewportSize({ width: 1024, height: 768 })
  await page.getByRole('button', { name: '展开侧栏', exact: true }).click()
  await page.reload()
  await expect(page.getByRole('searchbox', { name: '搜索功能' })).toBeVisible()
  await expect(page.getByRole('button', { name: '收起侧栏', exact: true })).toBeVisible()
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  await page.getByRole('button', { name: '收起侧栏', exact: true }).click()
  await page.setViewportSize({ width: 1366, height: 768 })
  await page.reload()
  await expect(page.getByRole('button', { name: '展开侧栏', exact: true })).toBeVisible()
  await page.getByRole('button', { name: '搜索功能', exact: true }).click()
  await expect(page.getByRole('searchbox', { name: '搜索功能', exact: true })).toBeFocused()
})

test('Token 构成区分已记录总量、部分缓存、同批实报样本和疑似测试，来源展开不额外请求', async ({ page }) => {
  await prepare(page)
  const trendRequests: string[] = []
  page.on('request', (request) => {
    const url = new URL(request.url())
    if (url.pathname === '/admin/stats/trend') trendRequests.push(url.pathname + url.search)
  })
  const source = (tokens: number | null, requests = 0) => ({ tokens, requests })
  await page.route('**/admin/stats/trend?*', (route) => route.fulfill({ json: { data: [], total: {
    requests: 221, prompt_tokens: 32300, completion_tokens: 41500, cached_tokens: 11420, reasoning_tokens: 0,
    cache_write_tokens: null, recorded_cache_write_tokens: 720, cache_read_known_requests: 208, cache_write_known_requests: 17,
    cache_hit_bp: null, measured_cache_hit_bp: 6415, measured_cache_hit_requests: 17, measured_prompt_tokens: 5300, measured_cache_read_tokens: 3400,
    suspected_test_requests: 210, suspected_test_tokens: 70000, test_detection_basis: 'fixture_model_name',
    token_provenance: {
      prompt: { upstream: source(5300, 17), estimated: source(1000, 4), local_override: source(0), unknown: source(26000, 200) },
      completion: { upstream: source(3000, 17), estimated: source(2000, 4), local_override: source(0), unknown: source(36500, 200) },
    },
  } } }))
  await page.goto('/admin')
  const panel = page.getByRole('region', { name: 'Token 用量构成', exact: true })
  await showDistribution(page, 'tokens')
  await expect(panel.getByTitle('73,800', { exact: true })).toHaveText('73,800 Tokens')
  await expect(panel).toContainText('已记录用量')
  await expect(panel).toContainText('输入 32,300 + 输出 41,500')
  await expect(panel.locator('[data-slot="token-segments"] dd')).toHaveText(['20,160', '11,420部分', '720部分', '41,500', '—'])
  const coverage = panel.getByRole('group', { name: '缓存采集覆盖率', exact: true })
  await expect(coverage).toContainText('208 / 221 · 94.11%')
  await expect(coverage).toContainText('17 / 221 · 7.69%')
  await expect(coverage).toContainText('64.15%')
  await expect(coverage).toContainText('样本 17 / 221 次')
  await expect(panel).toContainText('疑似测试 210 次 · 70,000 Token（仍计入）')
  await expect(panel.locator('summary')).toContainText('上游实报 8,300 · 来源未知 62,500')
  await panel.locator('summary').click()
  const provenance = panel.getByRole('group', { name: '用量来源', exact: true })
  await expect(provenance).toContainText('本地估算')
  await expect(provenance).toContainText('3,000')
  await expect(provenance).toContainText('本地覆盖')
  await expect(provenance).toContainText('输入 26,000 / 输出 36,500')
  await expect(panel).not.toContainText('NaN')
  expect(trendRequests.filter((url) => !url.includes('fields=core'))).toHaveLength(1)
  expect(trendRequests).toHaveLength(2)
  await page.screenshot({ path: 'test-results/dashboard-token-quality.png', fullPage: true, animations: 'disabled' })
  await panel.screenshot({ path: 'test-results/token-quality-panel.png', animations: 'disabled' })
})

test('Token 旧接口缺少缓存及来源字段时显示未知，不把零计数当作明确上报', async ({ page }) => {
  await prepare(page)
  await page.route('**/admin/stats/trend?*', (route) => route.fulfill({ json: { data: [], total: {
    requests: 2, prompt_tokens: 42, completion_tokens: 310, cached_tokens: 0, reasoning_tokens: 0,
  } } }))
  await page.goto('/admin')
  const panel = page.getByRole('region', { name: 'Token 用量构成', exact: true })
  await showDistribution(page, 'tokens')
  await expect(panel.getByTitle('352', { exact: true })).toHaveText('352 Tokens')
  await expect(panel.locator('[data-slot="token-segments"] dd')).toHaveText(['42', '—', '—', '310', '—'])
  await expect(panel).not.toContainText('100.0%')
  await expect(panel).not.toContainText('疑似测试')
  await panel.locator('summary').click()
  await expect(panel).toContainText('当前接口未提供完整用量来源')
})

test('Token 明确上报零缓存时展示 0，完整覆盖率与未知状态分开', async ({ page }) => {
  await prepare(page)
  await page.route('**/admin/stats/trend?*', (route) => route.fulfill({ json: { data: [], total: {
    requests: 2, prompt_tokens: 600, completion_tokens: 20, cached_tokens: 0, reasoning_tokens: 0,
    cache_read_known_requests: 2, cache_write_known_requests: 2, cache_write_tokens: 0, recorded_cache_write_tokens: 0,
    measured_cache_hit_bp: 0, measured_cache_hit_requests: 2, measured_prompt_tokens: 600, measured_cache_read_tokens: 0,
    token_detail_observations: { reasoning_tokens: { observed_tokens: 0, observed_records: 2, complete: true } },
  } } }))
  await page.goto('/admin')
  const panel = page.getByRole('region', { name: 'Token 用量构成', exact: true })
  await showDistribution(page, 'tokens')
  await expect(panel.locator('[data-slot="token-segments"] dd')).toHaveText(['600', '0', '0', '20', '0'])
  await expect(panel.getByRole('group', { name: '缓存采集覆盖率', exact: true })).toContainText('2 / 2 · 100.0%')
  await expect(panel).not.toContainText('缓存数据未完整上报')
  await expect(panel).not.toContainText('部分')
})

test('Token 来源缺失数值不补零，英文说明和窄屏保持可读', async ({ page }) => {
  await prepare(page, 'en')
  const unknown = { requests: 2, tokens: null }, zero = { requests: 0, tokens: 0 }
  await page.route('**/admin/stats/trend?*', (route) => route.fulfill({ json: { data: [], total: {
    requests: 2, prompt_tokens: 42, completion_tokens: 310,
    token_provenance: { prompt: { upstream: unknown, estimated: zero, local_override: zero, unknown }, completion: { upstream: zero, estimated: zero, local_override: zero, unknown } },
  } } }))
  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto('/admin')
  const panel = page.getByRole('region', { name: 'Token usage mix', exact: true })
  await showDistribution(page, 'tokens')
  await expect(panel).toContainText('Recorded usage')
  await expect(panel.locator('summary')).toContainText('Upstream reported — · unknown source —')
  await panel.locator('summary').click()
  await expect(panel).not.toContainText('NaN')
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
})

test('Dashboard 单卡切换模型、渠道与 Token 构成，占比包含榜外用量，明细保留时间窗', async ({ page }) => {
  const requests = await prepare(page)
  await page.goto('/admin?days=30')
  const models = page.getByRole('region', { name: '模型消费排行', exact: true })
  const channels = page.getByRole('region', { name: '渠道消费排行', exact: true })
  const tokens = page.getByRole('region', { name: 'Token 用量构成', exact: true })
  await expect(models.getByRole('listitem')).toHaveCount(5)
  await expect(models.getByRole('listitem').first()).toContainText('25.0%')
  await expect(models.getByRole('listitem').first().getByRole('link')).toHaveAccessibleName(/2,500 次请求/)
  await expect(models).toContainText('全部消费 US$10.00')
  await expect(models.getByRole('link', { name: /gpt-5.1/ })).toHaveAttribute('href', '/admin/stats?days=30&model=gpt-5.1')
  await showDistribution(page, 'channel')
  await expect(channels.getByRole('listitem')).toHaveCount(5)
  await expect(channels.getByRole('link', { name: /OpenAI Primary/ })).toHaveAttribute('href', '/admin/stats?days=30&channel_id=1')
  await expect(channels.getByRole('link', { name: '查看全部', exact: true })).toHaveAttribute('href', '/admin/stats?days=30&view=breakdown&by=channel')
  await showDistribution(page, 'tokens')
  await expect(tokens.getByTitle('600,000', { exact: true })).toHaveText('60万 Tokens')
  // 五段互斥，缓存与推理不能在输入/输出基础上再重复累计。
  await expect(tokens.locator('[data-slot="token-segments"] dd')).toHaveText(['24万', '14万', '20,000', '17万', '30,000'])
  expect(requests.filter(isUsage)).toHaveLength(1)
  expect(requests.filter(isChart)).toHaveLength(1)
  expect(requests.filter((url) => url.startsWith('/admin/stats/breakdown'))).toEqual([
    '/admin/stats/breakdown?days=30&by=model&limit=5&cached=true&fields=core&compare=false', '/admin/stats/breakdown?days=30&by=channel&limit=5&cached=true&fields=core&compare=false',
  ])
  await showDistribution(page, 'model')
  await expect(models.getByRole('button', { name: /再看|收起/ })).toHaveCount(0)
  await expect(models.getByRole('listitem')).toHaveCount(5)
  await expect(models.getByRole('link', { name: '查看全部', exact: true })).toHaveAttribute('href', '/admin/stats?days=30&view=breakdown&by=model')
  await showDistribution(page, 'channel')
  await expect(channels.getByRole('listitem')).toHaveCount(5)
  expect(requests.filter((url) => url.startsWith('/admin/stats/breakdown'))).toHaveLength(2)
  await page.getByRole('button', { name: '刷新', exact: true }).click()
  await expect.poll(() => requests.filter((url) => url.startsWith('/admin/stats/breakdown')).length).toBe(4)
  expect(requests.filter(isUsage)).toHaveLength(2)
  expect(requests.filter(isChart)).toHaveLength(2)
  await page.getByRole('button', { name: '近 7 天', exact: true }).click()
  await showDistribution(page, 'model')
  await expect(models).toContainText('近 7 天')
  await expect(models.getByRole('link', { name: /gpt-5.1/ })).toHaveAttribute('href', '/admin/stats?days=7&model=gpt-5.1')
  await models.getByRole('link', { name: /gpt-5.1/ }).click()
  await expect(page).toHaveURL(/days=7&model=gpt-5.1/)
  await page.goBack()
  await expect(models.getByRole('listitem')).toHaveCount(5)
})

test('Dashboard 排行加载、空态和失败分开，缺失名称可读、无渠道不能下钻到错误对象', async ({ page }) => {
  await prepare(page)
  let release: () => void = () => undefined
  const pending = new Promise<void>((resolve) => { release = resolve })
  await page.route('**/admin/stats/breakdown?*', async (route) => {
    const by = new URL(route.request().url()).searchParams.get('by')
    if (by === 'model') { await pending; return route.fulfill({ status: 500, json: { error: { code: 'internal_error' } } }) }
    return route.fulfill({ json: { total_amount_micro: 0, total_requests: 30, data: [
      { key: '42', label: null, channel_id: 42, rank: 1, amount_micro: 0, share_bp: 0, requests: 20 },
      { key: '0', label: null, channel_id: 0, rank: 2, amount_micro: 0, share_bp: 0, requests: 10 },
    ] } })
  })
  await page.goto('/admin')
  const models = page.getByRole('region', { name: '模型消费排行', exact: true })
  const channels = page.getByRole('region', { name: '渠道消费排行', exact: true })
  await expect(models.getByRole('status')).toBeVisible()
  await showDistribution(page, 'channel')
  await expect(channels.getByRole('link', { name: /渠道名称不可用（ID 42）/ })).toHaveAttribute('href', '/admin/stats?days=7&channel_id=42')
  await expect(channels.getByText('未分配渠道', { exact: true })).toBeVisible()
  await expect(channels.getByRole('link', { name: /未分配渠道/ })).toHaveCount(0)
  await expect(channels.getByRole('listitem').first()).toContainText('—')
  await expect(channels).toContainText('前 2 项占比 —')
  release()
  await showDistribution(page, 'model')
  await expect(models.getByRole('alert')).toBeVisible()
  await page.route('**/admin/stats/breakdown?*', (route) => route.fulfill({ json: { data: [], total_amount_micro: 0, total_requests: 0 } }))
  await models.getByRole('button', { name: '重试' }).click()
  await expect(models.getByRole('alert')).toHaveCount(0)
  await expect(models.getByText('窗口内还没有调用记录。')).toBeVisible()
})

test('窄屏长说明保持在视口内，鼠标可移入阅读，键盘能读取和关闭', async ({ page }) => {
  await prepare(page)
  await page.setViewportSize({ width: 320, height: 740 })
  await page.goto('/admin')
  const help = page.getByRole('button', { name: '概览口径', exact: true })
  const tooltip = page.getByRole('tooltip').filter({ hasText: '同一日期范围' })
  await help.hover()
  await expect(tooltip).toBeVisible()
  const box = await tooltip.boundingBox()
  expect(box!.x).toBeGreaterThanOrEqual(8)
  expect(box!.x + box!.width).toBeLessThanOrEqual(312)
  expect(box!.y).toBeGreaterThanOrEqual(8)
  expect(box!.y + box!.height).toBeLessThanOrEqual(732)
  expect(await tooltip.evaluate((node) => node.scrollWidth <= node.clientWidth)).toBe(true)
  await tooltip.hover()
  await page.waitForTimeout(180)
  await expect(tooltip).toBeVisible()
  await page.mouse.move(1, 1)
  await expect(tooltip).toHaveCount(0)
  await help.focus()
  await expect(help).toHaveAccessibleDescription(/主数字、费用与调用质量、趋势和排行使用同一日期范围/)
  await expect(tooltip).toBeVisible()
  await page.screenshot({ path: 'test-results/tooltip-mobile.png', animations: 'disabled' })
  await help.press('Escape')
  await expect(tooltip).toHaveCount(0)
  await expect(help).toBeFocused()
})

test('Dashboard 首屏资源与连接状态复用请求，异常说明支持键盘，资源可直接进入管理', async ({ page }) => {
  const requests = await prepare(page)
  await page.goto('/admin?days=30&scope=window')
  const inventory = page.getByRole('region', { name: '站点速览', exact: true })
  const healthSummary = page.getByRole('region', { name: '系统连接状态', exact: true })
  await expect(inventory).toBeInViewport({ ratio: 1 })
  await expect(healthSummary).toBeInViewport({ ratio: 1 })
  await expect(inventory.getByRole('link')).toHaveCount(4)
  for (const [label, value, href] of [
    ['用户', '1,258', '/admin/users'], ['活跃密钥', '2,856', '/admin/keys'],
    ['可用渠道', '22 / 24', '/admin/channels'], ['模型', '136', '/admin/pricing'],
  ]) {
    const link = inventory.getByRole('link', { name: new RegExp(`^${label} `) })
    await expect(link).toContainText(value)
    await expect(link).toHaveAttribute('href', href)
  }
  await expect(healthSummary.getByLabel('PG: 连接正常', { exact: true })).toBeVisible()
  const channels = inventory.getByRole('link', { name: /^可用渠道 / })
  await channels.focus()
  await expect(page.getByRole('tooltip')).toHaveText('22 / 24 有可用 key · 2 条已自动停用')
  await expect(channels).toHaveAccessibleDescription('22 / 24 有可用 key · 2 条已自动停用')
  await channels.press('Escape')
  await expect(page.getByRole('tooltip')).toHaveCount(0)
  expect(requests.filter((url) => url === '/admin/diagnose')).toHaveLength(1)
  expect(requests.filter((url) => url === '/admin/stats/inventory')).toHaveLength(1)
  await page.getByRole('button', { name: '刷新', exact: true }).click()
  await expect.poll(() => requests.filter((url) => url === '/admin/diagnose').length).toBe(2)
  expect(requests.filter((url) => url === '/admin/stats/inventory')).toHaveLength(2)
  await channels.focus()
  await channels.press('Enter')
  await expect(page).toHaveURL('/admin/channels')
  await page.goBack()
  await expect(page).toHaveURL('/admin?days=30&scope=window')
})

test('Dashboard 顶部概况加载和失败不显示零值或正常状态，恢复后同步更新', async ({ page }) => {
  await prepare(page)
  let release: () => void = () => undefined
  const pending = new Promise<void>((resolve) => { release = resolve })
  for (const endpoint of ['/admin/diagnose', '/admin/stats/inventory']) await page.route(`**${endpoint}`, async (route) => {
    await pending
    await route.fulfill({ status: 500, json: { error: { code: 'internal_error' } } })
  })
  await page.goto('/admin')
  const inventory = page.getByRole('region', { name: '站点速览', exact: true })
  const healthSummary = page.getByRole('region', { name: '系统连接状态', exact: true })
  try {
    await expect(inventory.getByRole('status')).toBeVisible()
    await expect(inventory.getByRole('link')).toHaveCount(0)
    await expect(healthSummary.getByRole('status')).toHaveText('检查中')
    await expect(healthSummary.getByLabel(/连接正常/)).toHaveCount(0)
  } finally { release() }
  await expect(inventory.getByRole('alert')).toBeVisible()
  await expect(inventory.getByRole('link')).toHaveCount(0)
  await expect(healthSummary.getByRole('link', { name: '系统状态待确认' })).toHaveAttribute('href', '#dashboard-attention')
  await page.unroute('**/admin/stats/inventory')
  await page.route('**/admin/diagnose', (route) => route.fulfill({ json: { ...health, clickhouse: null, nats_connected: false } }))
  await page.getByRole('button', { name: '刷新', exact: true }).click()
  await expect(inventory.getByRole('link')).toHaveCount(4)
  await expect(healthSummary.getByLabel('PG: 连接正常', { exact: true })).toBeVisible()
  await expect(healthSummary.getByLabel('CH: 未启用或未连接', { exact: true })).toBeVisible()
  await expect(healthSummary.getByLabel('NATS: 未启用或未连接', { exact: true })).toBeVisible()
  await page.route('**/admin/diagnose', (route) => route.fulfill({ status: 500, json: { error: { code: 'internal_error' } } }))
  await page.getByRole('button', { name: '刷新', exact: true }).click()
  await expect(healthSummary.getByRole('link', { name: '系统状态待确认' })).toBeVisible()
  await expect(healthSummary.getByLabel(/连接正常/)).toHaveCount(0)
})

for (const width of [320, 390, 1280, 1440]) {
  test(`Dashboard：${width}px 布局与长金额不溢出`, async ({ page }) => {
    await page.setViewportSize({ width, height: width === 1280 ? 800 : 1000 })
    await prepare(page, width === 390 ? 'en' : 'zh-CN')
    await page.route('**/admin/models', (route) => route.fulfill({ json: { data: [{ model_name: 'demo-model', pricing_mode: null }] } }))
    await page.route('**/admin/stats/channels?*', (route) => route.fulfill({ json: { data: [{ channel_id: 1, name: 'OpenAI Primary', error_rate_bp: 760 }] } }))
    await page.goto(width === 1280 ? '/admin' : '/admin?scope=window')
    await expect(page.locator('.recharts-surface').last()).toBeVisible()
    await expect(page.getByRole('link', { name: width === 390 ? 'Detailed analysis' : '详细分析' })).toBeVisible()
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    if (width === 320) {
      const amount = page.getByRole('region', { name: '经营概览' }).getByTitle('US$168.00', { exact: true })
      expect(await amount.evaluate((node) => {
        const range = document.createRange()
        range.selectNodeContents(node)
        return range.getClientRects().length
      })).toBe(1)
    }
    if (width >= 1280) {
      // 首页优先显示用量、质量、实时和待办；下方趋势及分布可滚动完整查看。
      await expect(page.getByRole('region', { name: '实时流量' })).toBeInViewport({ ratio: 1 })
      await expect(page.getByRole('region', { name: '站点速览' })).toBeInViewport({ ratio: 1 })
      await expect(page.getByRole('region', { name: '系统连接状态' })).toBeInViewport({ ratio: 1 })
      await expect(page.getByRole('region', { name: '经营概览' })).toBeInViewport({ ratio: 1 })
      await expect(page.getByRole('link', { name: '模型尚未定价 · 3 项待处理', exact: true })).toBeInViewport({ ratio: 1 })
      await expect(page.getByRole('region', { name: '优先处理', exact: true })).toBeInViewport({ ratio: 1 })
      await expect(page.getByRole('region', { name: '费用与调用质量' }).locator('dl')).toBeInViewport({ ratio: 1 })
      for (const panel of [page.getByRole('group', { name: '请求量与收入趋势', exact: true }), page.getByRole('region', { name: '用量分布', exact: true })]) {
        await panel.scrollIntoViewIfNeeded()
        await expect(panel).toBeInViewport({ ratio: 1 })
      }
    }
    if (width === 390) await page.evaluate(() => document.documentElement.classList.add('dark'))
    await page.screenshot({ path: `test-results/dashboard-${width}.png`, fullPage: true, animations: 'disabled' })
  })
}

for (const width of [320, 390, 768, 1024, 1366]) {
  test(`Dashboard ${width}px 实时与资源置顶，其后依次展示经营指标和调用质量`, async ({ page }) => {
    await page.setViewportSize({ width, height: 800 })
    const requests = await prepare(page)
    await page.goto('/admin')
    const summary = page.getByRole('region', { name: '经营概览', exact: true })
    const realtime = page.getByRole('region', { name: '实时流量', exact: true })
    const operations = page.getByRole('region', { name: '费用与调用质量', exact: true })
    await expect(page.getByRole('main').getByRole('region').first()).toHaveAccessibleName('实时流量')
    await expect(realtime).toHaveCount(1)
    await expect(realtime.getByRole('region', { name: '站点速览', exact: true })).toBeVisible()
    await expect(realtime.getByRole('region', { name: '优先处理', exact: true })).toBeVisible()
    await expect(realtime).toBeInViewport({ ratio: 1 })
    await expect(summary.getByRole('link')).toHaveCount(5)
    const [liveBox, summaryBox, operationsBox] = await Promise.all([realtime, summary, operations].map((panel) => panel.boundingBox()))
    expect(liveBox!.y + liveBox!.height).toBeLessThanOrEqual(summaryBox!.y)
    expect(summaryBox!.y + summaryBox!.height).toBeLessThanOrEqual(operationsBox!.y)
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    expect(requests.filter((url) => url.startsWith('/admin/stats/realtime?'))).toHaveLength(1)
    await page.screenshot({ path: test.info().outputPath(`dashboard-realtime-first-${width}.png`), animations: 'disabled' })
    await summary.scrollIntoViewIfNeeded()
    await expect(summary).toBeInViewport({ ratio: 1 })
  })
}

test('Dashboard 排行直接展示请求量，免费调用有数量且不能伪造消费占比', async ({ page }) => {
  const requests = await prepare(page)
  await page.route('**/admin/stats/breakdown?*', (route) => route.fulfill({ json: {
    total_amount_micro: 0, total_requests: 1234567, data: [
      { rank: 1, key: 'free-model', label: 'Example free model', channel_id: 9, amount_micro: 0, share_bp: 0, requests: 1234567 },
    ],
  } }))
  await page.goto('/admin?days=30')
  for (const name of ['模型消费排行', '渠道消费排行']) {
    await showDistribution(page, name === '模型消费排行' ? 'model' : 'channel')
    const ranking = page.getByRole('region', { name, exact: true })
    const row = ranking.getByRole('listitem')
    await expect(row.getByTitle('1,234,567 次请求', { exact: true })).toHaveText('123万 次')
    await expect(row).toContainText('US$0.00')
    await expect(row).toContainText('—')
    await expect(row).not.toContainText('0%')
  }
  // 请求次数取自已返回的排行，不追加实体或日志查询。
  expect(requests.filter((url) => url.startsWith('/admin/logs'))).toHaveLength(0)
})

test('Dashboard 首屏直接呈现资源增量和异常数，停用与无密钥同时展示且摘要口径一致', async ({ page }) => {
  const requests = await prepare(page)
  await page.setViewportSize({ width: 1366, height: 768 })
  await page.route('**/admin/stats/inventory', (route) => {
    requests.push('/admin/stats/inventory')
    return route.fulfill({ json: {
      users: { total: 1258, active: 1100, new_today: 16, new_7d: 56 }, api_keys: { total: 3210, active: 2856, used_7d: 1560 },
      channels: { total: 24, healthy: 20, no_key: 2, auto_disabled: 2 }, models: { total: 136, priced: 132, served: 128 },
    } })
  })
  await page.goto('/admin')
  const inventory = page.getByRole('region', { name: '站点速览', exact: true })
  for (const text of ['今日 +16', '7 天用过 1,560', '2 停用 · 2 无密钥', '4 未定价']) {
    await expect(inventory.getByText(text, { exact: true })).toBeInViewport({ ratio: 1 })
  }
  await expect(page.getByRole('tooltip')).toHaveCount(0)
  await expect(page.getByRole('link', { name: '模型尚未定价 · 2 项待处理', exact: true })).toBeVisible()
  await expect(page.getByRole('link', { name: '暂无待办', exact: true })).toHaveCount(0)
  const attention = page.locator('#dashboard-attention')
  await expect(attention).toContainText('4 个模型未配置定价')
  const channelAction = attention.getByRole('link').filter({ hasText: '2 条已自动停用；2 条启用但无可用 key' })
  await expect(channelAction).toHaveAttribute('href', '/admin/channels')
  expect(requests.filter((url) => url === '/admin/stats/inventory')).toHaveLength(1)
  await page.screenshot({ path: 'test-results/dashboard-resource-summary-1366.png', animations: 'disabled' })
})

test('Dashboard 资源汇总刷新失败后，待办不能把缺失的渠道状态报为正常', async ({ page }) => {
  await prepare(page)
  await page.goto('/admin')
  await expect(page.getByRole('link', { name: '模型尚未定价 · 2 项待处理', exact: true })).toBeVisible()
  await page.route('**/admin/stats/inventory', (route) => route.fulfill({ status: 500, json: { error: { code: 'internal_error' } } }))
  await page.getByRole('button', { name: '刷新', exact: true }).click()
  await expect(page.getByRole('link', { name: '部分状态待确认', exact: true })).toBeVisible()
  await expect(page.getByRole('region', { name: '站点速览', exact: true }).getByRole('link')).toHaveCount(0)
  await expect(page.locator('#dashboard-attention').getByRole('link')).toHaveCount(0)
  await expect(page.getByText('没有待办，一切正常。')).toHaveCount(0)
  await page.route('**/admin/stats/inventory', (route) => route.fulfill({ json: {
    users: { total: 1, active: 1, new_today: 0, new_7d: 0 }, api_keys: { total: 0, active: 0, used_7d: 0 },
    channels: { total: 0, healthy: 0, no_key: 0, auto_disabled: 0 }, models: { total: 0, priced: 0, served: 0 },
  } }))
  await page.getByRole('button', { name: '刷新', exact: true }).click()
  await expect(page.getByRole('link', { name: '暂无待办', exact: true })).toBeVisible()
  const channels = page.getByRole('region', { name: '站点速览', exact: true }).getByRole('link', { name: /^可用渠道/ })
  await expect(channels).toContainText('暂无可用')
  await expect(channels).not.toContainText('可调用')
})

test('首页排行按全量指标取榜，两个排行独立，详情与返回保留所选指标', async ({ page }) => {
  const requests = await prepare(page)
  await page.route('**/admin/stats/breakdown?*', (route) => {
    const url = new URL(route.request().url())
    const metric = url.searchParams.get('metric')
    if (!metric) return route.fallback()
    requests.push(url.pathname + url.search)
    const channel = url.searchParams.get('by') === 'channel'
    return route.fulfill({ json: { total_requests: 10000, total_tokens: 1000000, total_amount_micro: 10000000, data: [{
      rank: 1, key: channel ? '77' : 'free-high-volume', label: channel ? 'Token intensive channel' : null,
      channel_id: channel ? 77 : undefined, requests: 9000, request_share_bp: 9000, tokens: 700000, token_share_bp: 7000, amount_micro: 0, share_bp: 0,
    }] } })
  })
  await page.goto('/admin?days=30')
  await page.getByRole('combobox', { name: '模型排行指标' }).selectOption('requests')
  const models = page.getByRole('region', { name: '模型请求排行', exact: true })
  await expect(models.getByRole('listitem')).toHaveCount(1)
  await expect(models.getByRole('listitem')).toContainText('90.0%')
  await expect(models.getByRole('listitem').getByRole('link')).toHaveAttribute('href', '/admin/stats?days=30&measure=requests&model=free-high-volume')
  await showDistribution(page, 'channel')
  await expect(page.getByRole('combobox', { name: '渠道排行指标' })).toHaveValue('amount')
  await page.getByRole('combobox', { name: '渠道排行指标' }).selectOption('tokens')
  const channels = page.getByRole('region', { name: '渠道 Token 排行', exact: true })
  await expect(channels).toContainText('70万 Tokens')
  await expect(channels).toContainText('70.0%')
  await showDistribution(page, 'model')
  await models.getByRole('link', { name: '查看全部' }).click()
  await expect(page.getByRole('combobox', { name: '排序指标' })).toHaveValue('requests')
  await expect.poll(() => requests.includes('/admin/stats/breakdown?days=30&by=model&limit=50&metric=requests')).toBe(true)
  await page.goBack()
  await expect(page.getByRole('combobox', { name: '模型排行指标' })).toHaveValue('requests')
  await showDistribution(page, 'channel')
  await expect(page.getByRole('combobox', { name: '渠道排行指标' })).toHaveValue('tokens')
  await showDistribution(page, 'model')
  await page.reload()
  await expect(models).toContainText('free-high-volume')
  const before = requests.filter((url) => url.includes('limit=5&metric=')).length
  await page.getByRole('button', { name: '刷新', exact: true }).click()
  await expect.poll(() => requests.filter((url) => url.includes('limit=5&metric=')).length).toBe(before + 2)
  expect(requests.filter((url) => url.startsWith('/admin/stats/margin'))).toHaveLength(0)
})

test('Token 趋势共用汇总数据，支持小时口径、补零、导出与刷新恢复', async ({ page }) => {
  const requests = await prepare(page)
  await page.route('**/admin/stats/trend?*', (route) => {
    const url = new URL(route.request().url())
    requests.push(url.pathname + url.search)
    return route.fulfill({ json: { days: 1, granularity: 'hour', window: { start_at: '2026-09-26 00:00:00', end_at: '2026-09-26 02:15:00', timezone: 'UTC' }, total: { requests: 3 }, data: [
      { bucket: '2026-09-26 00:00:00', requests: 2, amount_micro: 1000000, tokens: 4000 },
      { bucket: '2026-09-26 02:00:00', requests: 1, amount_micro: 2000000, tokens: 2000 },
    ] } })
  })
  await page.goto('/admin?days=1')
  await expect(page.getByRole('region', { name: '经营概览' })).toContainText('42 次失败')
  await page.getByRole('group', { name: '图表指标' }).getByRole('button', { name: 'Token', exact: true }).click()
  await expect(page.getByText('小时均值').locator('..')).toContainText('2,000')
  await expect(page.getByText('小时峰值').locator('..')).toContainText('4,000 · 09-26 00:00:00')
  const trend = page.getByRole('group', { name: 'Token 用量趋势', exact: true })
  await trend.getByRole('button', { name: '数据表' }).click()
  await expect(trend.locator('tbody tr')).toHaveCount(3)
  await expect(trend.locator('tbody tr').nth(1).locator('td')).toHaveText(['2026-09-26 01:00:00', '0'])
  const download = page.waitForEvent('download')
  await trend.getByRole('button', { name: '导出 CSV' }).click()
  expect(await readFile((await (await download).path())!, 'utf8')).toContain('2026-09-26 00:00:00,4000')
  expect(requests.filter(isChart)).toHaveLength(1)
  expect(requests.filter(isUsage)).toHaveLength(1)
  await page.reload()
  await expect(page.getByRole('group', { name: '图表指标' }).getByRole('button', { name: 'Token', exact: true })).toHaveAttribute('aria-pressed', 'true')
})

test('四项待办直接集中在实时区域，服务恢复后资源异常仍保留，不再显示重复栏目', async ({ page }) => {
  const requests = await prepare(page)
  await page.route('**/admin/diagnose', (route) => {
    requests.push('/admin/diagnose')
    return route.fulfill({ json: { ...health, redis: false, cooling_keys: 3 } })
  })
  await page.goto('/admin?days=30')
  const actions = page.getByRole('region', { name: '优先处理', exact: true })
  await expect(actions).toBeInViewport({ ratio: 1 })
  await expect(actions.getByRole('link', { name: /Redis/ })).toHaveAttribute('href', '/admin/ops')
  await expect(actions.getByRole('link', { name: /3 把渠道 key/ })).toHaveAttribute('href', '/admin/channels')
  await expect(actions.getByRole('link')).toHaveCount(4)
  await expect(actions).toContainText('模型尚未定价')
  await expect(page.getByRole('heading', { name: /^(需要注意|站点规模)$/ })).toHaveCount(0)
  await expect(page.getByRole('region', { name: '站点速览', exact: true })).toHaveCount(1)
  expect(requests.filter((url) => url === '/admin/diagnose')).toHaveLength(1)
  await page.route('**/admin/diagnose', (route) => route.fulfill({ json: health }))
  await page.getByRole('button', { name: '刷新', exact: true }).click()
  await expect(actions.getByRole('link')).toHaveCount(2)
  await expect(actions).not.toContainText('服务连接异常')
  await expect(actions).not.toContainText('3 把渠道 key')
})

for (const width of [1024, 1440]) {
  test(`实时待办 ${width}px：四项横排、紧急优先，全部在原位展开并可键盘访问`, async ({ page }) => {
    const requests = await prepare(page)
    await page.setViewportSize({ width, height: 900 })
    await page.emulateMedia({ reducedMotion: 'reduce' })
    await page.addInitScript((dark) => localStorage.setItem('okapi.theme', dark ? 'dark' : 'light'), width === 1440)
    await page.route('**/admin/diagnose', (route) => route.fulfill({ json: { ...health, redis: false, dlq_depth: 3, outbox_pending: 1000, cooling_keys: 2 } }))
    await page.route('**/admin/pools', (route) => route.fulfill({ json: { data: [{ pool_code: 'empty-pool', channel_count: 0 }] } }))
    await page.route('**/admin/stats/channels?*', (route) => route.fulfill({ json: { data: [{ channel_id: 42, name: 'Primary', error_rate_bp: 700 }] } }))
    await page.route('**/admin/reconciliation', (route) => route.fulfill({ json: { drift_count: 1, drifts: [{ user_id: 2 }] } }))
    await page.goto('/admin?days=30&scope=window')
    const live = page.getByRole('region', { name: '实时流量', exact: true })
    const actions = live.getByRole('region', { name: '优先处理', exact: true })
    const links = actions.getByRole('link')
    await expect(links).toHaveCount(4)
    await expect(links.nth(0)).toHaveAccessibleName(/Redis 不可达/)
    await expect(links.nth(1)).toHaveAccessibleName(/死信队列有 3 条/)
    await expect(links.nth(2)).toHaveAccessibleName(/4 个模型未配置定价/)
    await expect(links.nth(3)).toHaveAccessibleName(/2 条已自动停用/)
    const boxes = await links.evaluateAll((nodes) => nodes.map((node) => { const b = node.getBoundingClientRect(); return { x: b.x, y: b.y, right: b.right } }))
    expect(new Set(boxes.map((b) => Math.round(b.y))).size).toBe(1)
    for (let i = 1; i < boxes.length; i++) expect(boxes[i].x).toBeGreaterThanOrEqual(boxes[i - 1].right)
    await expect(actions).toBeInViewport({ ratio: 1 })
    await expect(page.getByRole('heading', { name: /^(需要注意|站点规模)$/ })).toHaveCount(0)
    await expect(page.getByLabel('Redis: 连接异常', { exact: true })).toHaveCount(1)
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    await page.screenshot({ path: `test-results/dashboard-four-alerts-${width}.png`, animations: 'disabled' })
    await links.first().focus()
    await expect(page.getByRole('tooltip')).toContainText('付费请求已 fail-closed 拒绝')
    await expect(links.first()).toHaveAccessibleName(/付费请求已 fail-closed 拒绝/)
    await page.keyboard.press('Escape')
    const before = requests.length
    const all = actions.getByRole('button', { name: '全部 9 项', exact: true })
    await expect(all).toHaveAttribute('aria-expanded', 'false')
    await all.focus()
    await all.press('Enter')
    await expect(links).toHaveCount(9)
    const less = actions.getByRole('button', { name: '收起', exact: true })
    await expect(less).toHaveAttribute('aria-expanded', 'true')
    const listId = await less.getAttribute('aria-controls')
    await expect(page.locator(`[id="${listId}"]`).getByRole('link')).toHaveCount(9)
    await expect(actions.getByRole('link', { name: /渠道错误率超 5%/ })).toHaveAttribute('href', '/admin/quality?days=30&tab=channels')
    await expect(actions.getByRole('link', { name: /渠道池是空的/ })).toHaveAttribute('href', '/admin/pools')
    expect(requests.length).toBe(before)
    await less.press('Enter')
    await expect(links).toHaveCount(4)
    await all.press('Enter')
    await page.getByRole('button', { name: '近 7 天', exact: true }).click()
    await expect(links).toHaveCount(4)
    await expect(all).toHaveAttribute('aria-expanded', 'false')
  })
}

test('实时待办仅展示真实的零到四项，不补占位项，页头锚点在正常状态仍可定位', async ({ page }) => {
  await prepare(page)
  let count = 0
  await page.route('**/admin/diagnose', (route) => route.fulfill({ json: { ...health, redis: count < 1, dlq_depth: count >= 2 ? 1 : 0, outbox_pending: count >= 3 ? 1000 : 0, cooling_keys: count >= 4 ? 1 : 0 } }))
  await page.route('**/admin/stats/inventory', (route) => route.fulfill({ json: {
    users: { total: 1, active: 1, new_today: 0, new_7d: 0 }, api_keys: { total: 1, active: 1, used_7d: 1 },
    channels: { total: 1, healthy: 1, no_key: 0, auto_disabled: 0 }, models: { total: 1, priced: 1, served: 1 },
  } }))
  for (count = 0; count <= 4; count++) {
    await page.goto('/admin')
    const actions = page.locator('#dashboard-attention')
    await expect(actions.getByRole('link')).toHaveCount(count)
    await expect(actions.getByRole('button', { name: /全部/ })).toHaveCount(0)
    if (count === 0) {
      await expect(actions).toContainText('没有待办，一切正常。')
      await page.getByRole('link', { name: '暂无待办', exact: true }).click()
      await expect(actions).toBeFocused()
    } else await expect(actions).not.toContainText('没有待办，一切正常。')
  }
})

for (const width of [1024, 1440]) {
  test(`首页分布 ${width}px：单卡三个视图，最多五项且不足不补数据并与趋势上下对齐`, async ({ page }) => {
    await prepare(page)
    await page.setViewportSize({ width, height: 1000 })
    await page.emulateMedia({ reducedMotion: 'reduce' })
    await page.addInitScript((dark) => localStorage.setItem('okapi.theme', dark ? 'dark' : 'light'), width === 1440)
    let count = 1
    await page.route('**/admin/stats/breakdown?*', (route) => {
      const url = new URL(route.request().url()), channel = url.searchParams.get('by') === 'channel'
      expect(url.searchParams.get('limit')).toBe('5')
      return route.fulfill({ json: { total_requests: 17, total_amount_micro: 700, data: Array.from({ length: channel ? count + 1 : count }, (_, i) => ({
        key: channel ? String(i + 1) : `gpt-model-${i + 1}`, label: channel ? `Channel ${i + 1}` : null,
        rank: i + 1, channel_id: i + 1, requests: 17 - i, amount_micro: 700 - i, share_bp: 10000 - i,
      })) } })
    })
    await page.route('**/admin/stats/trend?*', (route) => route.fulfill({ json: {
      days: 7, window: { start_date: '2026-09-21', end_date: '2026-09-27', timezone: 'UTC' },
      data: [{ bucket: '2026-09-27', requests: 17, tokens: 352, amount_micro: 700 }],
      total: { requests: 17, errors: 14, prompt_tokens: 42, completion_tokens: 310, cached_tokens: 0, reasoning_tokens: 0, cache_write_tokens: null, cache_hit_bp: null },
    } }))
    const models = page.getByRole('region', { name: '模型消费排行', exact: true })
    const channels = page.getByRole('region', { name: '渠道消费排行', exact: true })
    const tokens = page.getByRole('region', { name: 'Token 用量构成', exact: true })
    const distribution = page.getByRole('region', { name: '用量分布', exact: true })
    const trend = page.locator('[data-slot="dashboard-trend"]')
    const checkAlignment = async () => {
      const [left, right] = await Promise.all([trend, distribution].map((panel) => panel.boundingBox()))
      expect(Math.abs(right!.y - left!.y)).toBeLessThanOrEqual(1)
      expect(Math.abs(right!.y + right!.height - left!.y - left!.height)).toBeLessThanOrEqual(1)
      await expect(distribution.getByRole('tabpanel')).toHaveCount(1)
      expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    }
    let previousHeight: number | undefined
    for (count of [1, 2, 3, 5, 6, 0]) {
      await page.goto('/admin')
      await expect(models.getByRole('listitem')).toHaveCount(Math.min(count, 5))
      await expect(models).toContainText('前 5 项')
      await expect(models.getByRole('button', { name: /再看|收起/ })).toHaveCount(0)
      if (count > 0) {
        expect((await models.getByRole('list').boundingBox())!.height).toBeGreaterThanOrEqual(160)
        const height = (await distribution.boundingBox())!.height
        if (previousHeight !== undefined) expect(Math.abs(height - previousHeight)).toBeLessThanOrEqual(1)
        previousHeight = height
      }
      await checkAlignment()
      await showDistribution(page, 'channel')
      await expect(channels.getByRole('listitem')).toHaveCount(Math.min(count + 1, 5))
      await checkAlignment()
      await showDistribution(page, 'tokens')
      await expect(tokens).toContainText('缓存数据未完整上报')
      await expect(tokens.locator('[data-slot="token-donut"]')).toBeVisible()
      await checkAlignment()
      if (count === 1 || count === 3) await page.screenshot({ path: test.info().outputPath(`dashboard-distribution-${width}-${count}-rows.png`), fullPage: true, animations: 'disabled' })
      await showDistribution(page, 'model')
    }
    await expect(models.getByText('窗口内还没有调用记录。')).toBeVisible()
    await page.route('**/admin/stats/breakdown?*', (route) => route.fulfill({ status: 500, json: { error: { code: 'internal_error' } } }))
    await page.getByRole('button', { name: '刷新', exact: true }).click()
    await expect(models.getByRole('alert')).toBeVisible()
    await expect(models.getByRole('listitem')).toHaveCount(0)
    await checkAlignment()
    await showDistribution(page, 'channel')
    await expect(channels.getByRole('alert')).toBeVisible()
    await checkAlignment()
  })
}

for (const width of [1024, 1280, 1920]) {
  test(`首页趋势图 ${width}px：绘图区撑满趋势卡，右侧更高时图下方不留空白`, async ({ page }) => {
    await prepare(page)
    await page.setViewportSize({ width, height: 900 })
    await page.goto('/admin')
    const trend = page.locator('[data-slot="dashboard-trend"]')
    const plot = trend.getByLabel('趋势绘图区')
    await expect(plot).toBeVisible()
    const [card, area, distribution] = await Promise.all([trend.boundingBox(), plot.boundingBox(), page.getByRole('region', { name: '用量分布', exact: true }).boundingBox()])
    // 右侧三个分布共用一卡；两侧保持等高，趋势绘图区随卡片撑满。
    expect(card!.y + card!.height - (area!.y + area!.height)).toBeLessThanOrEqual(12)
    expect(area!.height).toBeGreaterThanOrEqual(144)
    expect(Math.abs(distribution!.y + distribution!.height - card!.y - card!.height)).toBeLessThanOrEqual(1)
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  })
}

test('首页 768px 平板：五张指标卡 3+2 铺满两行，实时指标五项同排', async ({ page }) => {
  await prepare(page)
  await page.setViewportSize({ width: 768, height: 1024 })
  await page.goto('/admin')
  const cards = page.getByRole('region', { name: '经营概览' }).getByRole('link')
  await expect(cards).toHaveCount(5)
  const [a, b, c, d, e] = await Promise.all((await cards.all()).map((card) => card.boundingBox()))
  // 第一行三张等宽，第二行两张各占半行：右缘与第一行对齐，不留空位。
  expect(Math.abs(a!.y - b!.y)).toBeLessThanOrEqual(1)
  expect(Math.abs(b!.y - c!.y)).toBeLessThanOrEqual(1)
  expect(Math.abs(d!.y - e!.y)).toBeLessThanOrEqual(1)
  expect(d!.y).toBeGreaterThan(a!.y + a!.height)
  expect(Math.abs(d!.x - a!.x)).toBeLessThanOrEqual(1)
  expect(Math.abs(e!.x + e!.width - c!.x - c!.width)).toBeLessThanOrEqual(1)
  expect(Math.abs(d!.x + d!.width + 8 - e!.x)).toBeLessThanOrEqual(1)
  // 实时区五项一行，不再 3+2 折行。
  const live = page.getByRole('region', { name: '实时流量' })
  const ys = await Promise.all(['平均 QPS', '60 秒请求', '错误率', 'Token 数', '收入'].map(async (label) => (await live.getByText(label, { exact: true }).first().boundingBox())!.y))
  expect(Math.max(...ys) - Math.min(...ys)).toBeLessThanOrEqual(1)
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
})
