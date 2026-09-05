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
  await page.goto('/portal')
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

test('门户加载与失败不冒充零数据，旧缓存记录明确显示未采集', async ({ page }) => {
  await prepare(page)
  let release: () => void = () => undefined
  const gate = new Promise<void>((resolve) => { release = resolve })
  await page.route('**/api/me/stats/breakdown?*', async (route) => { await gate; await route.fulfill({ status: 501, json: { error: { code: 'stats_disabled' } } }) })
  await page.goto('/portal')
  await expect(page.getByRole('status')).toBeVisible()
  await expect(page.getByText(/暂无用量/)).toHaveCount(0)
  release()
  await expect(page.getByRole('alert')).toBeVisible()
  await page.unroute('**/api/me/stats/breakdown?*')
  await page.route('**/api/me/stats/breakdown?*', (route) => { const data = report(); data.total.cache_write_tokens = null; data.data.forEach((r) => { r.cache_write_tokens = null }); return route.fulfill({ json: data }) })
  await page.getByRole('button', { name: '重试', exact: true }).click()
  await page.getByRole('tab', { name: 'Token 构成' }).click()
  await expect(page.getByText(/部分历史记录未采集缓存写入/)).toBeVisible()
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
  await page.goto('/portal')
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
  await page.setViewportSize({ width: 390, height: 844 })
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  await page.evaluate(() => document.documentElement.classList.add('dark'))
  await page.screenshot({ path: 'test-results/flow-readable-names-mobile.png', fullPage: true, animations: 'disabled' })
})
