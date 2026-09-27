import { expect, test } from '@playwright/test'
import type { Page } from '@playwright/test'
import { fileURLToPath } from 'node:url'
import { calendarDays, usageChart } from '../src/features/portal-overview/usage-chart-data'
import type { BreakdownResp, BreakdownRow } from '../src/features/portal-overview/types'
import { trendChart } from '../src/features/analytics/trend-data'
import type { TrendResp } from '../src/features/analytics/types'

const row = (day: string, model: string, n = 1): BreakdownRow => ({ day, model, requests: 10 * n, prompt_tokens: 8000 * n, cached_tokens: 2000 * n, cache_write_tokens: 1000 * n,
  completion_tokens: 4000 * n, reasoning_tokens: 1000 * n, amount_micro: 50_000 * n, discount_micro: 10_000 * n, original_micro: 60_000 * n, errors: n,
  performance_requests: 10 * n, latency_sum_ms: 20_000 * n, ttft_sum_ms: 1500 * n, ttft_samples: 10 * n })

function report(start = '2026-08-29', end = '2026-09-04'): BreakdownResp {
  const data = calendarDays(start, end).flatMap((day, i) => i === 2 ? [] : [row(day, 'claude-sonnet-4.5', i + 1), row(day, 'gpt-5.1', 8 - i)])
  const sum = (field: keyof BreakdownRow) => data.reduce((n, r) => n + Number(r[field] ?? 0), 0)
  return { days: calendarDays(start, end).length, scope: 'key', live: { rpm: 12, tpm: 65000, rpd: 72, rpm_limit: 60, tpm_limit: 100000, rpd_limit: 1000 },
    window: { start_date: start, end_date: end, today: '2026-09-04', timezone: 'UTC', generated_at: '2026-09-04 10:00:00' }, data,
    total: { requests: sum('requests'), prompt_tokens: sum('prompt_tokens'), cached_tokens: sum('cached_tokens'), cache_write_tokens: sum('cache_write_tokens'), completion_tokens: sum('completion_tokens'), reasoning_tokens: sum('reasoning_tokens'),
      tokens: sum('prompt_tokens') + sum('completion_tokens'), amount_micro: sum('amount_micro'), discount_micro: sum('discount_micro'), cache_hit_bp: 2500, avg_rpm_micro: 53000, avg_tpm_micro: 630000000,
      success_rate_bp: 9000, avg_latency_ms: 2000, avg_ttft_ms: 150, tokens_per_1k_sec: 2000000 }, wallet_window_spend_micro: sum('amount_micro') }
}

function adminTrend(): TrendResp {
  return { days: 7, granularity: 'day', scope: {}, window: { start_at: '2026-08-29 00:00:00', end_at: '2026-09-04 12:00:00', timezone: 'UTC', generated_at: '2026-09-04 12:00:00', today: '2026-09-04', start_date: '2026-08-29', end_date: '2026-09-04', freshness: { last_event_at: '2026-09-04T11:59:00Z', last_ingested_at: '2026-09-04T12:00:00Z', pending_events: 2, failed_events: 0, queue_age_seconds: 90, event_gap_seconds: 90, stale: true, checked_at: '2026-09-04T12:01:00Z' } }, total: { requests: 5000, amount_micro: 3000000, tokens: 600000, errors: 100, error_rate_bp: 200, cache_hit_bp: 5000, avg_latency_ms: 1300 }, previous: { requests: 3000, amount_micro: 2500000, tokens: 400000 },
    data: calendarDays('2026-08-29', '2026-09-04').filter((_, i) => i !== 2).map((bucket, i) => ({ bucket, requests: 500 + 100 * i, errors: 10, error_rate_bp: 200, prompt_tokens: 40000, completion_tokens: 20000, cached_tokens: 20000, reasoning_tokens: 5000, tokens: 60000,
      cache_hit_bp: 5000, amount_micro: 400000, discount_micro: 20000, upstream_cost_micro: 300000, avg_latency_ms: 1200 + 100 * i, avg_ttft_ms: 200 + 20 * i, ttft_samples: 500, avg_output_tps_milli: 2000000 })) }
}

async function prepare(page: Page, language = 'zh-CN') {
  const queries: URL[] = []
  await page.addInitScript((language) => { localStorage.setItem('okapi.key', 'charts-test-key'); localStorage.setItem('okapi.lang', language) }, language)
  await page.route('**/*', async (route) => {
    const request = route.request()
    const url = new URL(request.url())
    if (request.isNavigationRequest()) return route.fulfill({ path: fileURLToPath(new URL('../dist/index.html', import.meta.url)), contentType: 'text/html' })
    if (!/^\/(api|admin|auth|pay)\//.test(url.pathname)) return route.continue()
    expect(request.method()).toBe('GET')
    queries.push(url)
    const json = url.pathname === '/api/me' ? { user_id: 1, key_id: 2, balance_micro: 180000000, role: 100, permissions: ['*'], group: 'default', balance_expires_at: null }
      : url.pathname === '/api/notice' ? { notice: null }
        : url.pathname === '/api/me/stats/breakdown' ? report(url.searchParams.get('start_date') ?? (url.searchParams.get('days') === '1' ? '2026-09-04' : undefined), url.searchParams.get('end_date') ?? undefined)
          : url.pathname === '/admin/stats/trend' ? adminTrend()
            : url.pathname === '/admin/stats/margin' ? { window: report().window, days: 7, total: { cost_known_requests: 4000, cost_coverage_bp: 8000, known_cost_micro: 2000000, known_margin_micro: 800000, requests: 5000, errors: 100, error_rate_bp: 200, amount_micro: 3000000, discount_micro: 200000, upstream_cost_micro: 2000000, margin_micro: 1000000, margin_rate_bp: 3333 }, data: calendarDays('2026-08-29', '2026-09-04').map((day, i) => ({ day, requests: 200 + i * 100, amount_micro: 200000 + i * 100000, discount_micro: 10000, upstream_cost_micro: 100000 })) }
              : url.pathname === '/admin/stats/cashflow' ? { today: { recharge_micro: 0, granted_micro: 0, clawed_micro: 0, expired_micro: 0 }, window: { recharge_micro: 0, granted_micro: 0, clawed_micro: 0, expired_micro: 0 } }
                : { data: [], next_before: null }
    await route.fulfill({ json })
  })
  return queries
}

test('稀疏日历补零、比例留空、模型名带点仍守恒，时延按样本加权', () => {
  const days = calendarDays('2024-02-28', '2024-03-01')
  expect(days).toEqual(['2024-02-28', '2024-02-29', '2024-03-01'])
  const rows = Array.from({ length: 8 }, (_, i) => row('2024-02-29', i === 0 ? '__proto__' : `model.${i}`, i + 1))
  const labels = { total: 'Total', latency: 'Latency', ttft: 'TTFT' }
  const chart = usageChart(rows, days, 'tokens', 'Other', labels)
  expect(chart.series).toHaveLength(7)
  expect(chart.data[0].s0).toBe(0)
  expect(chart.series.reduce((sum, s) => sum + Number(chart.data[1][s.key]), 0)).toBe(432000)
  expect(usageChart(rows, days, 'success', 'Other', labels).data[0].value).toBeNull()
  expect(usageChart(rows, days, 'latency', 'Other', labels).data[1].value).toBe(2000)
  rows[0].performance_requests = 0
  expect(usageChart(rows, days, 'latency', 'Other', labels).data[1].value).toBeNull()
  const trend = trendChart(adminTrend(), 'error_rate', 'Error rate', 'TTFT', 'Other', 'Unknown')
  expect(trend.data.find((r) => r.bucket === '2026-08-31')?.value).toBeNull()
})

test('门户图表七种指标、样式、图例与精确数据表可用，切换不重复请求', async ({ page }) => {
  const requests = await prepare(page)
  await page.goto('/portal?view=trend')
  const plot = page.getByRole('group', { name: '用量趋势', exact: true })
  await expect(plot.locator('.recharts-surface').first()).toBeVisible()
  await expect(page.getByRole('group', { name: '图表指标' }).getByRole('button')).toHaveCount(7)
  await page.getByRole('group', { name: '图表指标' }).getByRole('button', { name: 'Token', exact: true }).click()
  await plot.getByRole('button', { name: '数据表' }).click()
  await expect(plot.locator('tbody tr')).toHaveCount(7)
  await expect(plot.locator('tbody tr').filter({ hasText: '2026-08-31' })).toContainText('0')
  const download = page.waitForEvent('download')
  await plot.getByRole('button', { name: '导出 CSV' }).click()
  expect((await download).suggestedFilename()).toMatch(/usage-chart.*csv$/)
  await plot.getByRole('button', { name: '数据表' }).click()
  await plot.getByRole('button', { name: '柱状图', exact: true }).click()
  await plot.getByRole('group', { name: '显示的序列' }).getByRole('button').first().click()
  await expect(plot.getByRole('group', { name: '显示的序列' }).getByRole('button').first()).toHaveAttribute('aria-pressed', 'false')
  expect(requests.filter((r) => r.pathname === '/api/me/stats/breakdown')).toHaveLength(1)
  await plot.getByRole('group', { name: '显示的序列' }).getByRole('button').first().click()
  await plot.getByRole('button', { name: '面积图', exact: true }).click()
  await page.evaluate(() => window.scrollTo(0, 0))
  await page.screenshot({ path: 'test-results/usage-charts-desktop.png', fullPage: true, animations: 'disabled' })
})

test('门户趋势和模型分布各自保留指标，切签、刷新、后退和概览展开不会重置', async ({ page }) => {
  const queries = await prepare(page)
  await page.goto('/portal?scope=user&start_date=2026-09-01&end_date=2026-09-04&view=trend')
  const trendMetrics = page.getByRole('group', { name: '图表指标', exact: true })
  await trendMetrics.getByRole('button', { name: 'Token', exact: true }).click()
  await expect(page).toHaveURL(/measure=tokens/)
  await page.getByRole('tab', { name: '模型分布', exact: true }).click()
  const distribution = page.getByRole('group', { name: '分布指标', exact: true })
  await distribution.getByRole('button', { name: '请求数', exact: true }).click()
  await page.getByRole('tab', { name: 'Token 构成', exact: true }).click()
  await page.goBack()
  await expect(page.getByRole('tabpanel', { name: '模型分布', exact: true })).toBeVisible()
  await expect(distribution.getByRole('button', { name: '请求数', exact: true })).toHaveAttribute('aria-pressed', 'true')
  expect(queries.filter((url) => url.pathname === '/api/me/stats/breakdown')).toHaveLength(1)
  await page.reload()
  await expect(distribution.getByRole('button', { name: '请求数', exact: true })).toHaveAttribute('aria-pressed', 'true')
  await page.getByRole('tab', { name: '综合概览', exact: true }).click()
  await expect(page.getByRole('heading', { name: '用量趋势Token', exact: true })).toBeVisible()
  await page.getByRole('button', { name: '详细趋势', exact: true }).click()
  await expect(trendMetrics.getByRole('button', { name: 'Token', exact: true })).toHaveAttribute('aria-pressed', 'true')
  await expect(page).toHaveURL(/scope=user/)
  await expect(page).toHaveURL(/start_date=2026-09-01/)
  await page.getByRole('button', { name: '今天', exact: true }).click()
  await expect(page).not.toHaveURL(/start_date=/)
  await expect(trendMetrics.getByRole('button', { name: 'Token', exact: true })).toHaveAttribute('aria-pressed', 'true')
  await page.getByRole('tab', { name: '模型分布', exact: true }).click()
  await expect(distribution.getByRole('button', { name: '请求数', exact: true })).toHaveAttribute('aria-pressed', 'true')
  for (const request of queries.filter((url) => url.pathname === '/api/me/stats/breakdown')) {
    expect(request.searchParams.has('measure')).toBe(false)
    expect(request.searchParams.has('model_measure')).toBe(false)
  }
})

test('已用模型搜索支持联想、多词与全角输入，占比、排名不因过滤膨胀，日志返回恢复选择', async ({ page }) => {
  const queries = await prepare(page)
  await page.goto('/portal?scope=user&view=models&model_measure=tokens')
  const panel = page.getByRole('tabpanel', { name: '模型分布', exact: true })
  const table = panel.getByRole('table', { name: '模型用量分布', exact: true })
  await expect(table.locator('tbody tr')).toHaveCount(2)
  const claude = table.getByRole('row').filter({ hasText: 'claude-sonnet-4.5' })
  const before = await claude.getByRole('cell').allTextContents()
  const search = panel.getByRole('combobox', { name: '搜索已用模型', exact: true })
  await search.fill('ＣＬＡＵＤＥ ')
  await expect(search).toHaveValue('ＣＬＡＵＤＥ ')
  await search.pressSequentially('sonnet')
  await expect(search).toHaveValue('ＣＬＡＵＤＥ sonnet')
  await expect(table.locator('tbody tr')).toHaveCount(1)
  await expect(panel).toContainText('显示 1 / 2 个模型')
  await expect(claude.getByRole('cell')).toHaveText(before)
  await expect(page.getByRole('option')).toHaveCount(1)
  await search.press('ArrowDown')
  await search.press('Enter')
  await expect(search).toHaveValue('claude-sonnet-4.5')
  await expect(page).toHaveURL(/model_query=claude-sonnet-4\.5/)
  expect(queries.filter((url) => url.pathname === '/api/me/stats/breakdown')).toHaveLength(1)
  expect(queries.some((url) => url.pathname === '/api/pricing' || url.pathname === '/admin/models')).toBe(false)
  await table.getByRole('link', { name: '查看 claude-sonnet-4.5 的调用明细', exact: true }).click()
  await expect(page).toHaveURL(/\/portal\/logs\?scope=user/)
  await page.goBack()
  await expect(search).toHaveValue('claude-sonnet-4.5')
  await expect(panel.getByRole('group', { name: '分布指标' }).getByRole('button', { name: 'Token', exact: true })).toHaveAttribute('aria-pressed', 'true')
  await page.reload()
  await expect(search).toHaveValue('claude-sonnet-4.5')
  await panel.getByRole('button', { name: '清空', exact: true }).click()
  await expect(search).toBeFocused()
  await expect(table.locator('tbody tr')).toHaveCount(2)
  await expect(page).not.toHaveURL(/model_query=/)
})

test('已用模型无匹配时可清除搜索，非法指标安全回退且不影响日期范围', async ({ page }) => {
  const queries = await prepare(page)
  await page.goto('/portal?scope=user&view=models&start_date=2026-09-01&end_date=2026-09-04&measure=invalid&model_measure=cache&model_query=not-used')
  const panel = page.getByRole('tabpanel', { name: '模型分布', exact: true })
  await expect(panel).toContainText('显示 0 / 2 个模型')
  await expect(panel.getByRole('table')).toHaveCount(0)
  await expect(panel.getByRole('button', { name: '实际消费', exact: true })).toHaveAttribute('aria-pressed', 'true')
  await panel.getByRole('button', { name: '清除模型搜索', exact: true }).click()
  await expect(panel.getByRole('table').locator('tbody tr')).toHaveCount(2)
  await expect(page).toHaveURL(/start_date=2026-09-01/)
  await expect(page).toHaveURL(/scope=user/)
  await page.getByRole('tab', { name: '用量趋势', exact: true }).click()
  await expect(page.getByRole('group', { name: '图表指标' }).getByRole('button', { name: '实际消费', exact: true })).toHaveAttribute('aria-pressed', 'true')
  expect(queries.filter((url) => url.pathname === '/api/me/stats/breakdown')).toHaveLength(1)
})

for (const width of [320, 390]) {
  test(`模型分析 ${width}px：搜索候选和长模型名称不溢出，表格保持内部横向滚动`, async ({ page }) => {
    const english = width === 390
    await prepare(page, english ? 'en' : 'zh-CN')
    await page.setViewportSize({ width, height: 800 })
    if (english) await page.addInitScript(() => localStorage.setItem('okapi.theme', 'dark'))
    await page.route('**/api/me/stats/breakdown?*', (route) => {
      const data = report()
      data.data = data.data.map((row) => ({ ...row, model: `${row.model}-extended-context-application-specific-model` }))
      return route.fulfill({ json: data })
    })
    await page.goto('/portal?view=models&model_measure=requests')
    const panel = page.getByRole('tabpanel', { name: english ? 'By model' : '模型分布', exact: true })
    const search = panel.getByRole('combobox', { name: english ? 'Search used models' : '搜索已用模型', exact: true })
    await search.fill('claude')
    await expect(page.getByRole('option')).toHaveCount(1)
    const box = await page.getByRole('listbox').boundingBox()
    expect(box!.x).toBeGreaterThanOrEqual(0)
    expect(box!.x + box!.width).toBeLessThanOrEqual(width)
    await search.press('Escape')
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    await expect(panel.getByRole('table').locator('tbody tr')).toHaveCount(1)
    const viewport = panel.getByRole('table').locator('..')
    expect(await viewport.evaluate((node) => node.scrollWidth > node.clientWidth)).toBe(true)
    await panel.getByRole('button', { name: english ? 'View columns to the right' : '查看右侧列', exact: true }).click()
    expect(await viewport.evaluate((node) => node.scrollLeft > 0)).toBe(true)
    await panel.getByRole('button', { name: english ? 'View columns to the left' : '查看左侧列', exact: true }).click()
    await panel.screenshot({ path: `test-results/portal-model-explore-${width}.png`, animations: 'disabled' })
  })
}

test('自定义范围应用后查询，模型占比切换及缓存写入展示齐全', async ({ page }) => {
  const requests = await prepare(page)
  await page.goto('/portal')
  await page.getByText('自定义日期', { exact: true }).click()
  await page.getByLabel('开始日期').fill('2026-09-01')
  await page.getByLabel('结束日期').fill('2026-09-04')
  expect(requests.filter((r) => r.pathname === '/api/me/stats/breakdown')).toHaveLength(1)
  await page.getByRole('button', { name: '应用日期', exact: true }).click()
  await expect.poll(() => requests.filter((r) => r.pathname === '/api/me/stats/breakdown').at(-1)?.search).toContain('start_date=2026-09-01&end_date=2026-09-04')
  await page.getByRole('tab', { name: '模型分布' }).click()
  await page.getByRole('group', { name: '分布指标' }).getByRole('button', { name: 'Token', exact: true }).click()
  await expect(page.getByRole('columnheader', { name: 'Tokens', exact: true })).toBeVisible()
  await page.getByRole('tab', { name: 'Token 构成' }).click()
  await expect(page.getByRole('columnheader', { name: '缓存写入' })).toBeVisible()
  await expect(page.getByText('缓存写入', { exact: true }).first()).toBeVisible()
  await page.getByLabel('结束日期').fill('2026-08-30')
  await expect(page.getByRole('button', { name: '应用日期', exact: true })).toBeDisabled()
})

test('门户默认综合视图同时展示趋势与构成，展开明细保留范围且复用数据', async ({ page }) => {
  const queries = await prepare(page)
  await page.goto('/portal?scope=user&start_date=2026-09-01&end_date=2026-09-04')
  await expect(page.getByRole('tab', { name: '综合概览', exact: true })).toHaveAttribute('aria-selected', 'true')
  const models = page.getByRole('region', { name: '模型消费排行', exact: true })
  const tokens = page.getByRole('region', { name: 'Token 用量构成', exact: true })
  await expect(models.getByRole('listitem')).toHaveCount(2)
  await expect(tokens).toContainText('缓存写入')
  await expect(page.getByLabel('趋势绘图区')).toBeVisible()
  await page.getByRole('button', { name: '详细趋势', exact: true }).click()
  await expect(page.getByRole('group', { name: '图表指标' }).getByRole('button')).toHaveCount(7)
  await expect(page).toHaveURL(/scope=user/)
  await expect(page).toHaveURL(/start_date=2026-09-01/)
  await page.goBack()
  await models.getByRole('button', { name: '全部 2 个模型' }).click()
  await expect(page.getByRole('columnheader', { name: '每次均价' })).toBeVisible()
  await page.getByRole('tab', { name: '综合概览', exact: true }).click()
  await tokens.getByRole('button', { name: '构成明细' }).click()
  await expect(page.getByRole('columnheader', { name: '缓存写入' })).toBeVisible()
  expect(queries.filter((url) => url.pathname === '/api/me/stats/breakdown')).toHaveLength(1)
  await page.reload()
  await expect(page.getByRole('tab', { name: 'Token 构成', exact: true })).toHaveAttribute('aria-selected', 'true')
})

test('门户下钻调用明细保留日期和统计时区，排行与模型分布可键盘进入对应模型', async ({ page }) => {
  const queries = await prepare(page)
  await page.route('**/api/me/stats/breakdown?*', (route) => {
    const url = new URL(route.request().url())
    const data = report(url.searchParams.get('start_date') ?? undefined, url.searchParams.get('end_date') ?? undefined)
    data.window!.timezone = 'America/Los_Angeles'
    return route.fulfill({ json: data })
  })
  const start = '/portal?scope=user&start_date=2026-09-01&end_date=2026-09-04'
  await page.goto(start)
  const details = page.getByRole('link', { name: '调用明细', exact: true })
  await expect(details).toHaveAttribute('href', '/portal/logs?scope=user&start_date=2026-09-01&end_date=2026-09-04&timezone=America%2FLos_Angeles')
  await details.click()
  await expect.poll(() => queries.filter((url) => url.pathname === '/api/me/logs').at(-1)?.searchParams.get('end_date')).toBe('2026-09-04')
  await expect(page.getByRole('region', { name: '统计时段', exact: true })).toContainText('America/Los_Angeles')
  await page.goBack()
  await expect(page).toHaveURL(start)
  const ranking = page.getByRole('region', { name: '模型消费排行' })
  const modelLink = ranking.getByRole('link', { name: '查看 gpt-5.1 的调用明细', exact: true })
  await modelLink.focus()
  await modelLink.press('Enter')
  await expect(page.getByRole('combobox', { name: '模型', exact: true })).toHaveValue('gpt-5.1')
  await expect.poll(() => queries.filter((url) => url.pathname === '/api/me/logs').at(-1)?.searchParams.get('model')).toBe('gpt-5.1')
  const request = queries.filter((url) => url.pathname === '/api/me/logs').at(-1)!
  expect(Object.fromEntries(request.searchParams)).toMatchObject({ scope: 'user', start_date: '2026-09-01', end_date: '2026-09-04', timezone: 'America/Los_Angeles' })
  await page.goBack()
  await page.getByRole('tab', { name: '模型分布', exact: true }).click()
  await page.getByRole('table', { name: '模型用量分布' }).getByRole('link', { name: '查看 claude-sonnet-4.5 的调用明细', exact: true }).click()
  await expect(page).toHaveURL(/model=claude-sonnet-4\.5/)
  await expect(page).toHaveURL(/start_date=2026-09-01/)
  await page.goBack()
  await expect(page.getByRole('tab', { name: '模型分布', exact: true })).toHaveAttribute('aria-selected', 'true')
  await page.goto('/portal')
  await expect(details).toHaveAttribute('href', '/portal/logs?scope=user&start_date=2026-08-29&end_date=2026-09-04&timezone=America%2FLos_Angeles')
})

test('综合构成按全量计算前三名占比，缺失缓存写入不显示零', async ({ page }) => {
  await prepare(page)
  await page.route('**/api/me/stats/breakdown?*', (route) => {
    const data = report()
    data.data = [1, 2, 3, 4, 5].map((n) => row('2026-09-04', `model-${n}`, n))
    data.total.amount_micro = 750000
    data.total.cache_write_tokens = null
    return route.fulfill({ json: data })
  })
  await page.goto('/portal')
  const models = page.getByRole('region', { name: '模型消费排行', exact: true })
  await expect(models.getByRole('listitem')).toHaveCount(3)
  await expect(models.getByRole('listitem').first()).toContainText('model-5')
  await expect(models.getByRole('listitem').first()).toContainText('33.33%')
  await expect(models.getByRole('button', { name: '全部 5 个模型' })).toBeVisible()
  await expect(page.getByRole('region', { name: 'Token 用量构成' }).getByText('未采集', { exact: true })).toBeVisible()
})

test('门户筛选与视图刷新保留，前进后退同步日期草稿，预设日期清除自定义范围', async ({ page }) => {
  await prepare(page)
  await page.goto('/portal')
  await page.getByRole('button', { name: '全账户', exact: true }).click()
  await page.getByRole('button', { name: '近 90 天', exact: true }).click()
  await page.getByRole('tab', { name: 'Token 构成', exact: true }).click()
  await page.reload()
  await expect(page.getByRole('button', { name: '全账户', exact: true })).toHaveAttribute('aria-pressed', 'true')
  await expect(page.getByRole('button', { name: '近 90 天', exact: true })).toHaveAttribute('aria-pressed', 'true')
  await expect(page.getByRole('tab', { name: 'Token 构成', exact: true })).toHaveAttribute('aria-selected', 'true')
  await page.getByText('自定义日期', { exact: true }).click()
  await page.getByLabel('开始日期').fill('2026-09-01')
  await page.getByLabel('结束日期').fill('2026-09-03')
  await page.getByRole('button', { name: '应用日期', exact: true }).click()
  await expect(page).toHaveURL(/start_date=2026-09-01/)
  await page.getByLabel('开始日期').fill('2026-08-30')
  await page.getByRole('button', { name: '应用日期', exact: true }).click()
  await expect(page).toHaveURL(/start_date=2026-08-30/)
  await page.goBack()
  await expect(page.getByLabel('开始日期')).toHaveValue('2026-09-01')
  await page.goForward()
  await expect(page.getByLabel('开始日期')).toHaveValue('2026-08-30')
  await page.getByRole('button', { name: '今天', exact: true }).click()
  await expect(page).not.toHaveURL(/start_date/)
  await expect(page.getByLabel('开始日期')).toHaveValue('2026-09-04')
})

test('门户刷新更新用量和账户余额，刷新失败不把缓存的用量当作当前数据', async ({ page }) => {
  const requests = await prepare(page)
  await page.goto('/portal')
  const metrics = page.getByRole('region', { name: '用量概览', exact: true })
  await expect(metrics.getByTitle('US$2.70', { exact: true })).toBeVisible()
  const balances = requests.filter((r) => r.pathname === '/api/me').length
  const reports = requests.filter((r) => r.pathname === '/api/me/stats/breakdown').length
  await page.getByRole('button', { name: '刷新', exact: true }).click()
  await expect.poll(() => requests.filter((r) => r.pathname === '/api/me').length).toBe(balances + 1)
  await expect.poll(() => requests.filter((r) => r.pathname === '/api/me/stats/breakdown').length).toBe(reports + 1)
  await page.route('**/api/me/stats/breakdown?*', (route) => route.fulfill({ status: 500, json: { error: { code: 'internal_error' } } }))
  await page.getByRole('button', { name: '刷新', exact: true }).click()
  await expect(page.getByRole('alert')).toBeVisible()
  await expect(metrics.getByTitle('US$2.70', { exact: true })).toHaveCount(0)
  await expect(metrics.getByTitle('12 / 60', { exact: true })).toHaveCount(0)
  await expect(metrics.getByTitle('US$180.00', { exact: true })).toBeVisible()
})

for (const width of [390, 1280]) {
  test(`门户 ${width}px：已接入用户首屏概览、趋势和日期布局`, async ({ page }) => {
    await prepare(page)
    await page.setViewportSize({ width, height: 800 })
    await page.route('**/api/me/keys?*', (route) => route.fulfill({ json: { total: 1, data: [{ used_micro: 2700000, requests: 540, last_used_at: '2026-09-04T10:00:00Z' }] } }))
    await page.goto('/portal')
    await expect(page.getByRole('region', { name: '快速开始' })).toHaveCount(0)
    await expect(page.locator('.recharts-surface').first()).toBeVisible()
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    if (width === 1280) {
      await expect(page.getByRole('region', { name: '用量概览', exact: true })).toBeInViewport({ ratio: 1 })
      await expect(page.getByRole('region', { name: '调用质量', exact: true })).toBeInViewport({ ratio: 1 })
      await expect(page.getByLabel('趋势绘图区', { exact: true })).toBeInViewport({ ratio: 1 })
      await expect(page.getByRole('region', { name: '模型消费排行', exact: true })).toBeInViewport({ ratio: 1 })
      await expect(page.getByRole('region', { name: 'Token 用量构成', exact: true })).toBeInViewport({ ratio: 1 })
    }
    await page.screenshot({ path: `test-results/portal-overview-${width}.png`, fullPage: true, animations: 'disabled' })
  })
}

test('门户加载与失败不冒充零数据，旧缓存记录明确显示未采集', async ({ page }) => {
  await page.setViewportSize({ width: 320, height: 800 })
  await prepare(page)
  let release: () => void = () => undefined
  const gate = new Promise<void>((resolve) => { release = resolve })
  await page.route('**/api/me/stats/breakdown?*', async (route) => { await gate; await route.fulfill({ status: 501, json: { error: { code: 'stats_disabled' } } }) })
  await page.goto('/portal')
  await expect(page.getByRole('status')).toBeVisible()
  await expect(page.getByText(/暂无用量/)).toHaveCount(0)
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  const metrics = page.getByRole('region', { name: '用量概览', exact: true })
  expect(await metrics.evaluate((node) => [...node.querySelectorAll('.skeleton-shimmer')].every((placeholder) => placeholder.getBoundingClientRect().right <= node.getBoundingClientRect().right))).toBe(true)
  await page.screenshot({ path: 'test-results/portal-loading-320.png', fullPage: true, animations: 'disabled' })
  release()
  await expect(page.getByRole('alert')).toBeVisible()
  await page.unroute('**/api/me/stats/breakdown?*')
  await page.route('**/api/me/stats/breakdown?*', (route) => { const data = report(); data.total.cache_write_tokens = null; data.data.forEach((r) => { r.cache_write_tokens = null }); return route.fulfill({ json: data }) })
  await page.getByRole('button', { name: '重试', exact: true }).click()
  await page.getByRole('tab', { name: 'Token 构成' }).click()
  await expect(page.getByText(/部分请求或历史记录未上报缓存写入/)).toBeVisible()
})

test('手机和深色图表布局不溢出，提示框遵循深色主题', async ({ page }) => {
  await prepare(page)
  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto('/portal')
  await expect(page.locator('.recharts-surface').first()).toBeVisible()
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  const balance = page.getByTitle('US$180.00', { exact: true })
  expect(await balance.evaluate((node) => node.scrollWidth <= node.clientWidth)).toBe(true)
  await page.screenshot({ path: 'test-results/usage-charts-mobile.png', fullPage: true, animations: 'disabled' })
  await page.setViewportSize({ width: 1440, height: 1000 })
  await page.evaluate(() => document.documentElement.classList.add('dark'))
  await page.locator('.recharts-surface').first().hover({ position: { x: 400, y: 100 } })
  await expect(page.locator('.recharts-tooltip-wrapper .bg-popover')).toBeVisible()
  await page.evaluate(() => window.scrollTo(0, 0))
  await page.screenshot({ path: 'test-results/usage-charts-dark.png', fullPage: true, animations: 'disabled' })
})

test('单日数据有可见标记，管理统计失败不显示虚假的零指标', async ({ page }) => {
  await prepare(page)
  await page.goto('/portal?view=trend')
  await page.getByRole('group', { name: '统计时段' }).getByRole('button', { name: '今天', exact: true }).click()
  await expect(page.locator('.recharts-area-dot').first()).toBeVisible()
  await page.getByRole('group', { name: '图表指标' }).getByRole('button', { name: '平均时延', exact: true }).click()
  await expect(page.locator('.recharts-line-dot').first()).toBeVisible()
  await page.route('**/admin/stats/trend?*', (route) => route.fulfill({ status: 501, json: { error: { code: 'stats_disabled' } } }))
  await page.goto('/admin/stats')
  await expect(page.getByRole('alert')).toBeVisible()
  await expect(page.locator('main').getByText('—', { exact: true })).toHaveCount(6)
})

test('管理趋势、质量与经营报表共享图表交互并保留指标含义', async ({ page }) => {
  await prepare(page, 'en')
  await page.goto('/admin/stats')
  await page.getByRole('group', { name: 'Chart metric' }).getByRole('button', { name: 'Cache hit rate' }).click()
  await page.getByRole('button', { name: 'Data table' }).click()
  await expect(page.locator('tbody tr').filter({ hasText: '2026-08-31' })).toContainText('—')
  await page.goto('/admin/quality')
  await page.getByRole('button', { name: 'Output throughput', exact: true }).click()
  await expect(page.getByText('Unit: Token/s', { exact: true })).toBeVisible()
  await page.screenshot({ path: 'test-results/quality-charts-desktop.png', fullPage: true, animations: 'disabled' })
  await page.goto('/admin/revenue')
  await page.getByRole('button', { name: 'Data table' }).click()
  await expect(page.getByRole('table').first()).toContainText('$0.20')
  await page.getByRole('button', { name: 'Data table' }).click()
  await page.evaluate(() => window.scrollTo(0, 0))
  await page.screenshot({ path: 'test-results/revenue-charts-desktop.png', fullPage: true, animations: 'disabled' })
})

test('质量趋势的指标、比较维度与高级条件可刷新还原；草稿切指标不丢且应用前不参与查询', async ({ page }) => {
  const queries = await prepare(page)
  const latest = () => queries.findLast((url) => url.pathname === '/admin/stats/trend')!.searchParams
  const metrics = page.getByRole('group', { name: '图表指标', exact: true })
  await page.goto('/admin/quality')
  await metrics.getByRole('button', { name: '平均时延', exact: true }).click()
  await page.getByRole('combobox', { name: '对比维度', exact: true }).selectOption('model_group')
  await page.getByText('高级筛选与比较', { exact: true }).click()
  await page.getByLabel('请求端点', { exact: true }).fill('/v1/responses')
  await metrics.getByRole('button', { name: '输出吞吐量', exact: true }).click()
  await expect(page.getByLabel('请求端点', { exact: true })).toHaveValue('/v1/responses')
  await expect.poll(() => latest().get('metric')).toBe('throughput')
  expect(latest().get('endpoint')).toBeNull()
  await page.getByLabel('模型口径', { exact: true }).selectOption('upstream')
  await page.getByLabel('调用类型', { exact: true }).selectOption('stream')
  await page.getByLabel('比较模型（最多 8 个）').fill('up.a')
  await page.getByLabel('比较模型（最多 8 个）').press('Enter')
  await page.getByLabel('比较分组（最多 8 个）').fill('vip')
  await page.getByLabel('比较分组（最多 8 个）').press('Enter')
  await page.getByRole('button', { name: '应用分析条件', exact: true }).click()
  await expect.poll(() => latest().get('endpoint')).toBe('/v1/responses')
  for (const [key, value] of Object.entries({ stack: 'model_group', metric: 'throughput', model_source: 'upstream', request_type: 'stream', models: '["up.a"]', groups: '["vip"]' })) expect(latest().get(key)).toBe(value)
  await page.reload()
  await expect(metrics.getByRole('button', { name: '输出吞吐量', exact: true })).toHaveAttribute('aria-pressed', 'true')
  await expect(page.getByRole('combobox', { name: '对比维度', exact: true })).toHaveValue('model_group')
  await expect(page.locator('summary')).toContainText('/v1/responses')
  await expect(page.locator('summary')).toContainText('up.a')
  await page.getByText('高级筛选与比较', { exact: true }).click()
  await expect(page.getByLabel('模型口径', { exact: true })).toHaveValue('upstream')
  await page.getByRole('button', { name: '重置高级条件', exact: true }).click()
  await expect.poll(() => latest().get('endpoint')).toBeNull()
  for (const key of ['model_source', 'request_type', 'models', 'groups']) expect(latest().get(key)).toBeNull()
  expect(latest().get('metric')).toBe('throughput')
  expect(latest().get('stack')).toBe('model_group')
  await page.goBack()
  await expect.poll(() => latest().get('endpoint')).toBe('/v1/responses')
  await expect(page.getByLabel('请求端点', { exact: true })).toHaveValue('/v1/responses')
})

test('质量自定义时段不误亮预设；跨页签明确统计范围并保留筛选，选新天数仅替换日期', async ({ page }) => {
  const queries = await prepare(page)
  const params = new URLSearchParams({ days: '30', measure: 'ttft', stack: 'channel', start_date: '2026-09-01', end_date: '2026-09-04', node: 'gateway-east', stream: 'false', granularity: 'hour' })
  await page.goto(`/admin/quality?${params}`)
  await expect.poll(() => queries.findLast((url) => url.pathname === '/admin/stats/trend')?.searchParams.get('days')).toBe('4')
  const period = page.getByRole('group', { name: '统计时段', exact: true })
  await expect(period).toBeVisible()
  await expect(period.locator('[aria-pressed=true]')).toHaveCount(0)
  await expect(page.locator('summary')).toContainText('2026-09-01 — 2026-09-04')
  await page.getByRole('tab', { name: '错误分布', exact: true }).click()
  await expect(page.getByRole('note')).toContainText('近 30 天展示全站数据')
  const request = () => queries.findLast((url) => url.pathname === '/admin/stats/errors')
  await expect.poll(() => request()?.searchParams.get('days')).toBe('30')
  expect(request()?.searchParams.get('node')).toBeNull()
  expect(request()?.searchParams.get('start_date')).toBeNull()
  await page.reload()
  await expect(page.getByRole('note')).toContainText('趋势筛选已保留')
  await page.getByRole('link', { name: '返回质量趋势', exact: true }).click()
  await expect(page.getByRole('group', { name: '图表指标', exact: true }).getByRole('button', { name: '首 Token 时延', exact: true })).toHaveAttribute('aria-pressed', 'true')
  await expect(page.getByRole('combobox', { name: '对比维度', exact: true })).toHaveValue('channel')
  await expect(page.locator('summary')).toContainText('gateway-east')
  await page.getByRole('button', { name: '近 7 天', exact: true }).click()
  const latest = () => queries.findLast((url) => url.pathname === '/admin/stats/trend')!.searchParams
  await expect.poll(() => latest().get('days')).toBe('7')
  for (const [key, value] of Object.entries({ metric: 'ttft', stack: 'channel', node: 'gateway-east', stream: 'false', granularity: 'hour' })) expect(latest().get(key)).toBe(value)
  expect(latest().get('start_date')).toBeNull()
  expect(latest().get('end_date')).toBeNull()
  await page.goBack()
  await expect(page.locator('summary')).toContainText('2026-09-01 — 2026-09-04')
  await expect(period.locator('[aria-pressed=true]')).toHaveCount(0)
})

test('质量地址中的非法指标、比较维度和列表安全回退，false 条件仍参与查询', async ({ page }) => {
  const queries = await prepare(page)
  const params = new URLSearchParams({ days: '-1', measure: 'amount', stack: 'invalid', models: JSON.stringify(Array.from({ length: 9 }, (_, i) => `m${i}`)), groups: '[1]', stream: 'false' })
  await page.goto(`/admin/quality?${params}`)
  const latest = () => queries.findLast((url) => url.pathname === '/admin/stats/trend')!.searchParams
  await expect.poll(() => queries.findLast((url) => url.pathname === '/admin/stats/trend')?.searchParams.get('metric')).toBe('error_rate')
  expect(latest().get('days')).toBe('7')
  expect(latest().get('stream')).toBe('false')
  for (const key of ['stack', 'models', 'groups']) expect(latest().get(key)).toBeNull()
  await expect(page.getByRole('group', { name: '图表指标', exact: true }).getByRole('button', { name: '错误率', exact: true })).toHaveAttribute('aria-pressed', 'true')
  await expect(page.getByRole('combobox', { name: '对比维度', exact: true })).toHaveValue('')
  await expect(page.locator('summary')).toContainText('调用类型: 非流式')
  await expect(page.locator('summary')).not.toContainText('false')
  await page.getByText('高级筛选与比较', { exact: true }).click()
  await expect(page.getByLabel('调用类型', { exact: true })).toHaveValue('non_stream')
})

test('质量趋势切换比较维度恢复新序列，刷新失败不继续显示旧入库状态', async ({ page }) => {
  await prepare(page)
  await page.route('**/admin/stats/trend?*', (route) => {
    const stack = new URL(route.request().url()).searchParams.get('stack') ?? 'model'
    const data = adminTrend()
    data.stack = stack
    data.series = [{ key: 'a', label: `${stack}-A` }, { key: 'b', label: `${stack}-B` }]
    data.data = (data.data as import('../src/features/analytics/types').TrendBucket[]).map((row) => ({ bucket: row.bucket, values: { a: row, b: { ...row, avg_latency_ms: 900 } } }))
    return route.fulfill({ json: data })
  })
  await page.goto('/admin/quality?measure=latency&stack=model')
  const legend = page.getByRole('group', { name: '显示的序列', exact: true })
  await legend.getByRole('button', { name: 'model-A', exact: true }).click()
  await expect(legend.getByRole('button', { name: 'model-A', exact: true })).toHaveAttribute('aria-pressed', 'false')
  await page.getByRole('combobox', { name: '对比维度', exact: true }).selectOption('channel')
  await expect(legend.getByRole('button', { name: 'channel-A', exact: true })).toHaveAttribute('aria-pressed', 'true')
  await expect(page.locator('.recharts-line')).toHaveCount(2)
  await page.route('**/admin/stats/trend?*', (route) => route.fulfill({ status: 500, json: { error: { code: 'internal_error' } } }))
  await page.getByRole('combobox', { name: '对比维度', exact: true }).selectOption('model')
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误' })).toBeVisible()
  await expect(page.getByText('全站入库有延迟，近期数据可能尚未齐全。')).toHaveCount(0)
})

for (const width of [320, 390]) {
  test(`质量趋势 ${width}px：已选条件、页签范围提示和返回入口不溢出`, async ({ page }) => {
    await prepare(page, width === 390 ? 'en' : 'zh-CN')
    await page.setViewportSize({ width, height: 844 })
    const params = new URLSearchParams({ days: '30', measure: 'throughput', stack: 'model_group', tab: 'channels', endpoint: '/v1/very-long-endpoint-name-for-quality-comparison', models: '["very-long-model-name-for-trend-analysis"]' })
    await page.goto(`/admin/quality?${params}`)
    await expect(page.getByRole('note')).toBeVisible()
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    await page.getByRole('link', { name: width === 390 ? 'Return to quality trend' : '返回质量趋势', exact: true }).click()
    await expect(page.locator('summary')).toContainText('very-long-model-name-for-trend-analysis')
    await expect(page.locator('.recharts-surface').first()).toBeVisible()
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    if (width === 390) await page.evaluate(() => document.documentElement.classList.add('dark'))
    await page.screenshot({ path: `test-results/quality-trend-filters-${width}.png`, fullPage: true, animations: 'disabled' })
  })
}

test('性能比较保持各序列独立，空样本留空，组合名称可读', () => {
  const report = adminTrend()
  const sample = report.data[0] as import('../src/features/analytics/types').TrendBucket
  report.stack = 'model_group'
  const a = '["up.a","vip"]', b = '["up.b","default"]'
  report.series = [{ key: a, label: null }, { key: b, label: null }]
  report.data = [{ bucket: sample.bucket, values: { [a]: sample, [b]: { ...sample, requests: 2, avg_latency_ms: 3000, ttft_samples: 0 } } }]
  const plot = trendChart(report, 'latency', 'Latency', 'TTFT', 'Other', 'Unknown')
  expect(plot.stacked).toBe(false)
  expect(plot.line).toBe(true)
  expect(plot.series[0].label).toBe('up.a · vip')
  expect(plot.data[0].s0).toBe(sample.avg_latency_ms)
  expect(plot.data[0].s1).toBe(3000)
  expect(plot.data[1].s0).toBeNull()
  expect(trendChart(report, 'ttft', 'TTFT', 'TTFT', 'Other', 'Unknown').data[0].s1).toBeNull()
})

test('高级筛选提交后才查询，跨视图与刷新保留，模型分组支持性能对比', async ({ page }) => {
  const queries = await prepare(page)
  await page.route('**/admin/stats/trend?*', (route) => {
    const q = new URL(route.request().url()).searchParams
    const data = adminTrend()
    if (q.get('start_date') && q.get('end_date')) {
      const start = q.get('start_date')!, end = q.get('end_date')!
      data.days = calendarDays(start, end).length
      data.window = { ...data.window!, start_at: `${start} 00:00:00`, end_at: `${end} 23:00:00`, start_date: start, end_date: end }
      data.data = data.data.filter((r) => r.bucket >= start && r.bucket <= end)
    }
    if (q.get('stack') === 'model_group') {
      const key = '["up.a","vip"]', second = '["up.b","vip"]'
      data.stack = 'model_group'; data.series = [{ key, label: null }, { key: second, label: null }]
      data.data = (data.data as import('../src/features/analytics/types').TrendBucket[]).map((r) => ({ bucket: r.bucket, values: { [key]: r, [second]: { ...r, avg_latency_ms: r.avg_latency_ms * 2 - 750 } } }))
    }
    queries.push(new URL(route.request().url()))
    return route.fulfill({ json: data })
  })
  await page.goto('/admin/stats')
  await expect(page.getByText(/全站入库有延迟/)).toBeVisible()
  await page.getByText('高级筛选与比较', { exact: true }).click()
  const before = queries.filter((q) => q.pathname === '/admin/stats/trend').length
  await page.getByLabel('开始日期').fill('2026-09-01')
  await page.getByLabel('结束日期').fill('2026-09-04')
  await page.getByLabel('模型口径', { exact: true }).selectOption('upstream')
  await page.getByLabel('请求端点', { exact: true }).fill('/v1/responses')
  await page.getByLabel('调用类型', { exact: true }).selectOption('stream')
  await page.getByLabel('比较模型（最多 8 个）').fill('up.a')
  await page.getByLabel('比较模型（最多 8 个）').press('Enter')
  await page.getByLabel('比较模型（最多 8 个）').fill('up.b')
  await page.getByLabel('比较模型（最多 8 个）').press('Enter')
  await page.getByLabel('比较分组（最多 8 个）').fill('vip')
  await page.getByLabel('比较分组（最多 8 个）').press('Enter')
  expect(queries.filter((q) => q.pathname === '/admin/stats/trend')).toHaveLength(before)
  await page.getByRole('button', { name: '应用分析条件' }).click()
  await expect.poll(() => queries.at(-1)?.searchParams.get('model_source')).toBe('upstream')
  const last = queries.at(-1)!
  expect(last.searchParams.get('models')).toBe('["up.a","up.b"]')
  expect(last.searchParams.get('groups')).toBe('["vip"]')
  expect(last.searchParams.get('start_date')).toBe('2026-09-01')
  await page.getByText('高级筛选与比较', { exact: true }).click()
  await page.getByRole('group', { name: '图表指标' }).getByRole('button', { name: '平均时延', exact: true }).click()
  await page.getByRole('combobox', { name: '对比维度' }).selectOption('model_group')
  await expect(page.getByRole('button', { name: 'up.a · vip' })).toBeVisible()
  await page.getByRole('button', { name: '数据表', exact: true }).click()
  await expect(page.getByRole('columnheader', { name: 'up.a · vip' })).toBeVisible()
  await page.getByRole('tab', { name: '拆分' }).click()
  await expect.poll(() => queries.findLast((q) => q.pathname === '/admin/stats/breakdown')?.searchParams.get('endpoint')).toBe('/v1/responses')
  await page.reload()
  await expect(page.locator('summary')).toContainText('2026-09-01 — 2026-09-04')
  await page.getByRole('tab', { name: '趋势' }).click()
  await expect(page.locator('.recharts-line').first()).toBeVisible()
  await page.screenshot({ path: 'test-results/advanced-analysis-desktop.png', fullPage: true, animations: 'disabled' })
  await page.setViewportSize({ width: 390, height: 844 })
  await page.getByText('高级筛选与比较', { exact: true }).click()
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  await page.screenshot({ path: 'test-results/advanced-analysis-mobile.png', fullPage: true, animations: 'disabled' })
})

test('快速筛选支持模型别名，单模型取代旧比较列表，编号校验与中文输入法不误提交', async ({ page }) => {
  const queries = await prepare(page)
  await page.route('**/admin/models', (route) => route.fulfill({ json: { data: [{ model_name: 'gpt-5', display_name: '旗舰模型', vendor: 'OpenAI' }] } }))
  await page.goto('/admin/stats?models=%5B%22old-model%22%5D&user_id=7')
  const quick = page.getByRole('region', { name: '快速筛选', exact: true })
  const model = quick.getByRole('combobox', { name: '模型', exact: true })
  await model.fill('旗舰')
  await expect(page.getByRole('option', { name: /gpt-5.*旗舰模型/ })).toBeVisible()
  await model.press('ArrowDown')
  await model.press('Enter')
  await expect(model).toHaveValue('gpt-5')
  await expect(page).not.toHaveURL(/model=gpt-5/)
  await quick.getByRole('button', { name: '添加过滤' }).click()
  await expect(page).toHaveURL(/model=gpt-5/)
  await expect(page).not.toHaveURL(/models=/)
  await expect.poll(() => queries.findLast((q) => q.pathname === '/admin/stats/trend')?.searchParams.get('model')).toBe('gpt-5')
  expect(queries.findLast((q) => q.pathname === '/admin/stats/trend')?.searchParams.get('models')).toBeNull()
  await quick.getByRole('combobox', { name: '筛选维度' }).selectOption('user_id')
  const user = quick.getByRole('combobox', { name: '用户', exact: true })
  await user.fill('1e2')
  await expect(user).toHaveAttribute('aria-invalid', 'true')
  await expect(quick.getByRole('button', { name: '添加过滤' })).toBeDisabled()
  await user.fill('12')
  await user.dispatchEvent('keydown', { key: 'Enter', code: 'Enter', isComposing: true })
  await expect(page).toHaveURL(/user_id=7/)
  await user.press('Enter')
  await expect(page).toHaveURL(/user_id=12/)
  await quick.getByRole('button', { name: '移除过滤 gpt-5' }).click()
  await expect(page).not.toHaveURL(/model=/)
})

test('高级比较的模型候选去重、草稿隔离，手机内滚动保留应用按钮', async ({ page }) => {
  const queries = await prepare(page)
  await page.route('**/admin/models', (route) => route.fulfill({ json: { data: [
    { model_name: 'gpt-5', display_name: '旗舰模型', vendor: 'OpenAI' },
    { model_name: 'claude-sonnet-4.5', display_name: 'Sonnet', vendor: 'Anthropic' },
  ] } }))
  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto('/admin/stats?model=old-model')
  await page.getByText('高级筛选与比较', { exact: true }).click()
  const model = page.getByLabel('比较模型（最多 8 个）', { exact: true })
  await model.fill('OpenAI')
  await page.getByRole('option', { name: /gpt-5/ }).click()
  await expect(model).toHaveValue('')
  await model.fill('旗舰')
  await page.getByRole('option', { name: /gpt-5/ }).click()
  const editor = page.getByRole('region', { name: '编辑分析条件', exact: true })
  await expect(editor.getByRole('button', { name: '移除过滤 gpt-5' })).toHaveCount(1)
  await model.fill('Anthropic')
  await page.getByRole('option', { name: /claude-sonnet-4.5/ }).click()
  await expect(page).toHaveURL(/model=old-model/)
  expect(queries.findLast((q) => q.pathname === '/admin/stats/trend')?.searchParams.get('models')).toBeNull()
  await expect(page.getByRole('button', { name: '应用分析条件' })).toBeInViewport({ ratio: 1 })
  const bounds = await editor.boundingBox()
  const footer = await page.getByRole('button', { name: '应用分析条件' }).boundingBox()
  expect(bounds!.y + bounds!.height).toBeLessThanOrEqual(footer!.y)
  expect(await editor.evaluate((el) => el.scrollHeight > el.clientHeight)).toBe(true)
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  await page.screenshot({ path: 'test-results/analysis-editor-mobile.png', fullPage: true, animations: 'disabled' })
  await page.getByRole('button', { name: '应用分析条件' }).click()
  await expect(page).not.toHaveURL(/model=old-model/)
  await expect.poll(() => queries.findLast((q) => q.pathname === '/admin/stats/trend')?.searchParams.get('models')).toBe('["gpt-5","claude-sonnet-4.5"]')
})

test('窄屏指标可滚动发现，方向键仅定位，确认后才查询，变宽后收起滚动按钮', async ({ page }) => {
  const queries = await prepare(page)
  await page.setViewportSize({ width: 320, height: 740 })
  await page.goto('/admin/stats')
  const metrics = page.getByRole('group', { name: '图表指标', exact: true })
  const first = metrics.getByRole('button').first(), last = metrics.getByRole('button').last()
  const forward = page.getByRole('button', { name: '向后滚动图表指标', exact: true })
  await expect(forward).toBeVisible()
  await expect(first).toHaveAttribute('aria-pressed', 'true')
  const before = queries.filter((q) => q.pathname === '/admin/stats/trend').length
  await forward.click()
  await expect.poll(() => metrics.evaluate((node) => node.scrollLeft)).toBeGreaterThan(0)
  await expect(first).toHaveAttribute('aria-pressed', 'true')
  expect(queries.filter((q) => q.pathname === '/admin/stats/trend')).toHaveLength(before)
  await first.focus()
  await first.press('End')
  await expect(last).toBeFocused()
  await expect(first).toHaveAttribute('aria-pressed', 'true')
  expect(queries.filter((q) => q.pathname === '/admin/stats/trend')).toHaveLength(before)
  const bounds = await metrics.boundingBox(), selected = await last.boundingBox()
  expect(selected!.x).toBeGreaterThanOrEqual(bounds!.x)
  expect(selected!.x + selected!.width).toBeLessThanOrEqual(bounds!.x + bounds!.width + 1)
  expect(selected!.height).toBeGreaterThanOrEqual(44)
  await last.press('Enter')
  await expect(last).toHaveAttribute('aria-pressed', 'true')
  await expect.poll(() => queries.filter((q) => q.pathname === '/admin/stats/trend').length).toBe(before + 1)
  await expect(page).toHaveURL(/measure=throughput/)
  await page.screenshot({ path: 'test-results/choice-rail-mobile.png', fullPage: true, animations: 'disabled' })
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  await page.setViewportSize({ width: 1440, height: 900 })
  await expect(forward).toHaveCount(0)
  await expect(last).toHaveAttribute('aria-pressed', 'true')
})

test('流向隐藏阶段会重新查询路径，日期校验阻止误提交', async ({ page }) => {
  const queries = await prepare(page, 'en')
  await page.route('**/admin/stats/flow?*', (route) => {
    const url = new URL(route.request().url()); queries.push(url)
    const stages: string[] = JSON.parse(url.searchParams.get('stages') ?? '["user","node","api_key","group","model","channel"]')
    return route.fulfill({ json: { days: 7, metric: 'requests', scope: {}, stages, total: 50, coverage_bp: 10000, truncated: false, nodes: stages.map((stage) => ({ id: `${stage}:demo`, stage, key: 'demo', label: stage, value: 50, other: false })), links: stages.slice(1).map((s, i) => ({ source: `${stages[i]}:demo`, target: `${s}:demo`, value: 50 })) } })
  })
  await page.goto('/admin/stats?view=flow&metric=requests')
  await expect(page.locator('.recharts-surface')).toBeVisible()
  await page.getByRole('checkbox', { name: 'Gateway node' }).uncheck()
  await expect.poll(() => JSON.parse(queries.findLast((q) => q.pathname === '/admin/stats/flow')?.searchParams.get('stages') ?? '[]')).not.toContain('node')
  await page.getByText('Advanced filters & comparison', { exact: true }).click()
  await page.getByLabel('Start date').fill('2026-06-01')
  await page.getByLabel('End date').fill('2026-08-31')
  await page.getByLabel('Time granularity').selectOption('hour')
  const count = queries.length
  await page.getByRole('button', { name: 'Apply analysis filters' }).click()
  await expect(page.getByRole('alert')).toBeVisible()
  expect(queries.length).toBe(count)
  await page.getByRole('button', { name: 'Reset advanced filters' }).click()
  await page.getByText('Advanced filters & comparison', { exact: true }).click()
  await page.screenshot({ path: 'test-results/advanced-flow-desktop.png', fullPage: true, animations: 'disabled' })
})

test('流向名称可读、历史编号辅助显示，标题与五列数据对齐并支持键盘下钻', async ({ page }) => {
  const queries = await prepare(page)
  await page.route('**/admin/stats/flow?*', (route) => {
    const url = new URL(route.request().url()); queries.push(url)
    const nodes = [
      { id: 'user:7', stage: 'user', key: '7', label: '张三', entity_status: 'active', value: 9000 },
      { id: 'user:2528', stage: 'user', key: '2528', label: '#2528', value: 1000 },
      { id: 'api_key:8', stage: 'api_key', key: '8', label: '', entity_status: 'active', owner_name: '张三', key_prefix: 'sk-demo', value: 10000 },
      { id: 'group:default', stage: 'group', key: 'default', label: 'default', value: 10000 },
      { id: 'model:sonnet', stage: 'model', key: 'sonnet', label: 'Claude Sonnet', value: 10000 },
      { id: 'channel:9', stage: 'channel', key: '9', label: '海外主渠道', entity_status: 'deleted', provider: 'anthropic', value: 9000 },
      { id: 'channel:1631', stage: 'channel', key: '1631', label: null, entity_status: 'missing', value: 1000 },
    ]
    return route.fulfill({ json: { days: 7, metric: 'requests', scope: {}, stages: ['user', 'api_key', 'group', 'model', 'channel'], total: 10000, coverage_bp: 10000, truncated: false, nodes: nodes.map((n) => ({ ...n, other: false })), links: [
      { source: 'user:7', target: 'api_key:8', value: 9000 }, { source: 'user:2528', target: 'api_key:8', value: 1000 }, { source: 'api_key:8', target: 'group:default', value: 10000 }, { source: 'group:default', target: 'model:sonnet', value: 10000 }, { source: 'model:sonnet', target: 'channel:9', value: 9000 }, { source: 'model:sonnet', target: 'channel:1631', value: 1000 },
    ] } })
  })
  await page.goto('/admin/stats?view=flow&metric=requests')
  const plot = page.locator('.recharts-surface')
  await expect(plot.locator('[data-flow-name]').filter({ hasText: /^张三$/ })).toBeVisible()
  await expect(plot.locator('[data-flow-name]').filter({ hasText: /^历史用户$/ })).toBeVisible()
  await expect(plot.locator('[data-flow-name]').filter({ hasText: /^未命名密钥$/ })).toBeVisible()
  await expect(plot.locator('[data-flow-name]').filter({ hasText: /^默认分组$/ })).toBeVisible()
  await expect(plot.locator('[data-flow-name]').filter({ hasText: /^历史渠道$/ })).toBeVisible()
  await expect(plot.locator('[data-flow-name]').filter({ hasText: /^#/ })).toHaveCount(0)
  await expect(plot.locator('[data-flow-stage]')).toHaveCount(5)
  await expect(page.getByRole('checkbox', { name: '网关节点', exact: true })).not.toBeChecked()
  for (const stage of ['user', 'api_key', 'group', 'model', 'channel']) {
    const titleX = Number(await plot.locator(`[data-flow-stage="${stage}"] circle`).getAttribute('cx'))
    const barX = Number(await plot.locator(`[data-flow-node^="${stage}:"] .recharts-rectangle`).first().getAttribute('x'))
    // Rectangle renders as a path; compare its bounding box in SVG coordinates instead.
    const nodeX = await plot.locator(`[data-flow-node^="${stage}:"] .recharts-rectangle`).first().evaluate((node) => (node as SVGGraphicsElement).getBBox().x)
    expect(Math.abs(titleX - (Number.isFinite(barX) && barX !== 0 ? barX : nodeX) - 4)).toBeLessThan(1)
  }
  await expect(plot.locator('[data-flow-node="api_key:8"] title')).toContainText('张三 · sk-demo…')
  await expect(plot.locator('[data-flow-node="channel:9"] title')).toContainText('已删除')
  await page.screenshot({ path: 'test-results/flow-readable-names-desktop.png', fullPage: true, animations: 'disabled' })
  await page.getByText('节点明细 · 7', { exact: true }).click()
  await expect(page.getByRole('table')).toContainText('#2528')
  await expect(page.getByRole('table')).toContainText('anthropic')
  await page.getByText('节点明细 · 7', { exact: true }).click()
  await plot.locator('[data-flow-node="channel:9"]').focus()
  await page.keyboard.press('Enter')
  await expect.poll(() => queries.findLast((q) => q.pathname === '/admin/stats/flow')?.searchParams.get('channel_id')).toBe('9')

  await page.route('**/admin/stats/flow?*', (route) => {
    if (route.request().isNavigationRequest()) return route.fallback()
    return route.fulfill({ status: 500, json: { error: { code: 'internal_error' } } })
  })
  await page.getByRole('button', { name: '近 1 天' }).click()
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误，请稍后再试' })).toBeVisible()

  await page.route('**/admin/stats/flow?*', (route) => {
    if (route.request().isNavigationRequest()) return route.fallback()
    return route.fulfill({
      json: {
        days: 30, metric: 'requests', scope: {}, stages: ['user', 'api_key', 'group', 'model', 'channel'],
        total: 0, coverage_bp: 10000, truncated: false, nodes: [], links: [],
      },
    })
  })
  await page.getByRole('button', { name: '近 30 天' }).click()
  await expect(page.getByText('窗口内还没有调用记录。')).toBeVisible()

  await page.setViewportSize({ width: 390, height: 844 })
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  await page.evaluate(() => document.documentElement.classList.add('dark'))
  await page.screenshot({ path: 'test-results/flow-readable-names-mobile.png', fullPage: true, animations: 'disabled' })
})
