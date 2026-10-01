import { expect, test } from '@playwright/test'
import type { Page } from '@playwright/test'
import { fileURLToPath } from 'node:url'
import type { CubeMetrics, TrendResp } from '../src/features/analytics/types'
import { trendChart } from '../src/features/analytics/trend-data'

// Three calls: 100+50, 900+450 and 100+0 tokens. Durations are 1s, 9s
// and 0.5s. Two stream samples explicitly report TTFT=0. This checks
// presentation against independently calculated values, including valid zero.
const metrics: CubeMetrics = {
  requests: 3, errors: 1, error_rate_bp: 3333,
  prompt_tokens: 1100, cached_tokens: 180, completion_tokens: 500,
  reasoning_tokens: 100, tokens: 1600, cache_hit_bp: 1636,
  amount_micro: 3000, discount_micro: 750, upstream_cost_micro: 2000,
  avg_latency_ms: 3500, avg_ttft_ms: 0, ttft_samples: 2,
  avg_output_tps_milli: 47619,
}

function report(total = metrics): TrendResp {
  return {
    days: 7, granularity: 'day', scope: {},
    window: { start_at: '2026-09-24 00:00:00', end_at: '2026-09-30 23:59:59',
      start_date: '2026-09-24', end_date: '2026-09-30', timezone: 'America/Los_Angeles',
      generated_at: '2026-10-01T07:00:00Z' },
    total,
    previous: { requests: 1, amount_micro: -1000, avg_latency_ms: null },
    data: [{ bucket: '2026-09-30', ...total }],
  }
}

async function prepare(page: Page, getReport = () => report()) {
  await page.addInitScript(() => {
    localStorage.setItem('okapi.key', 'analytics-metrics-fixture-key')
    localStorage.setItem('okapi.lang', 'zh-CN')
  })
  await page.route('**/*', async (route) => {
    const request = route.request()
    const url = new URL(request.url())
    if (request.isNavigationRequest()) return route.fulfill({ path: fileURLToPath(new URL('../dist/index.html', import.meta.url)), contentType: 'text/html' })
    if (!/^\/(api|admin|auth|pay)\//.test(url.pathname)) return route.continue()
    expect(request.method()).toBe('GET')
    const json = url.pathname === '/api/me'
      ? { user_id: 1, key_id: 2, balance_micro: 1000000, role: 100, permissions: ['*'], group: 'default' }
      : url.pathname === '/api/notice' ? { notice: null }
        : url.pathname === '/admin/stats/trend' ? getReport()
          : url.pathname === '/admin/stats/breakdown' ? {
            days: 7, by: 'model', scope: {}, total_amount_micro: 3000, total_requests: 3, total_tokens: 1600,
            data: [{ ...metrics, avg_latency_ms: 0, key: 'measured-zero', label: null, rank: 1,
              previous_rank: null, previous_amount_micro: null, delta_bp: null, share_bp: 10000,
              request_share_bp: 10000, token_share_bp: 10000 }],
          } : { data: [], next_before: null }
    return route.fulfill({ json })
  })
}

test('分析 KPI 保留微额费用和有效零首字，未知上期时延不生成环比', async ({ page }) => {
  await prepare(page)
  await page.goto('/admin/stats')
  const card = (label: string) => page.getByText(label, { exact: true }).first().locator('..')
  await expect(card('请求数')).toContainText('3')
  await expect(card('消费')).toContainText('US$0.003')
  await expect(card('消费')).toContainText('+400%')
  await expect(card('Tokens')).toContainText('1,600')
  await expect(card('错误率')).toContainText('33.33%')
  await expect(card('缓存命中')).toContainText('16.36%')
  await expect(card('平均时延')).toContainText('3,500 ms')
  await expect(card('平均时延')).toContainText('首字 0 ms')
  await expect(card('平均时延')).not.toContainText('%')
  await expect(card('平均时延')).not.toContainText('新')
  await expect(page.locator('svg.recharts-surface')).toContainText('0.003')
  await page.screenshot({ path: '/private/tmp/okapi-analytics-metrics.png', fullPage: true })
})

test('分析拆分和趋势保留已测量的零时延，不将缺失测量补成零', async ({ page }) => {
  await prepare(page)
  await page.goto('/admin/stats?view=breakdown')
  const row = page.getByRole('row').filter({ hasText: 'measured-zero' })
  await expect(row).toContainText('0 ms')
  await expect(row).toContainText('首字 0 ms')
  const measured = report({ ...metrics, avg_latency_ms: 0 })
  const plot = (metric: 'latency' | 'ttft', input = measured) => trendChart(input, metric, metric, 'TTFT', 'Other', 'Unknown')
  expect(plot('latency').data.at(-1)?.value).toBe(0)
  expect(plot('ttft').data.at(-1)?.value).toBe(0)
  expect(plot('latency').data[0].value).toBeNull()
  const unknown = report({ ...metrics, avg_latency_ms: null, avg_ttft_ms: null, ttft_samples: 0 })
  expect(plot('latency', unknown).data.at(-1)?.value).toBeNull()
  expect(plot('ttft', unknown).data.at(-1)?.value).toBeNull()
})

test('分析 KPI 区分明确零缓存与未知缓存，缺失当前时延不显示负百分百', async ({ page }) => {
  let data = report({ ...metrics, cache_hit_bp: 0, cached_tokens: 0 })
  await prepare(page, () => data)
  await page.goto('/admin/stats')
  const cache = page.getByText('缓存命中', { exact: true }).first().locator('..')
  await expect(cache).toContainText('0.0%')
  data = report({ ...metrics, cache_hit_bp: null, avg_latency_ms: null, avg_ttft_ms: null, ttft_samples: 0 })
  data.previous.avg_latency_ms = 1000
  await page.reload()
  await expect(cache).toContainText('—')
  await expect(cache).not.toContainText('%')
  const latency = page.getByText('平均时延', { exact: true }).first().locator('..')
  await expect(latency).toContainText('—')
  await expect(latency).not.toContainText('%')
  await expect(latency).not.toContainText('首字 0')
})

test('消费流向保留净退款明细，不绘制丢掉负值的桑基图', async ({ page }) => {
  await prepare(page, () => report({ ...metrics, amount_micro: 6000 }))
  await page.route('**/admin/stats/flow?*', (route) => route.fulfill({ json: {
    days: 7, metric: 'amount', scope: {}, stages: ['model', 'channel'], total: 6000,
    coverage_bp: 10000, truncated: false,
    nodes: [
      { id: 'model:call', stage: 'model', key: 'call', label: null, other: false, value: 10000 },
      { id: 'model:refund', stage: 'model', key: 'refund', label: null, other: false, value: -4000 },
      { id: 'channel:1', stage: 'channel', key: '1', label: 'test-channel', entity_status: 'active', other: false, value: 6000 },
    ],
    links: [
      { source: 'model:call', target: 'channel:1', value: 10000 },
      { source: 'model:refund', target: 'channel:1', value: -4000 },
    ],
  } }))
  await page.goto('/admin/stats?view=flow')
  await expect(page.getByRole('note')).toContainText('该时段包含净退款')
  await expect(page.getByRole('row').filter({ hasText: 'refund' })).toContainText('-US$0.004')
  await expect(page.getByRole('row').filter({ hasText: 'test-channel' })).toContainText('US$0.006')
  await expect(page.locator('.recharts-sankey')).toHaveCount(0)
})

test('只有退款的拆分保留负金额，零调用不显示虚假的错误率和时延', async ({ page }) => {
  await prepare(page)
  await page.route('**/admin/stats/breakdown?*', (route) => route.fulfill({ json: {
    days: 7, by: 'model', scope: {}, total_amount_micro: -1000, total_requests: 0, total_tokens: 0,
    data: [{ ...metrics, key: 'refund-only', label: null, rank: 1, previous_rank: null,
      previous_amount_micro: null, delta_bp: null, requests: 0, errors: 0, error_rate_bp: 0,
      amount_micro: -1000, tokens: 0, cached_tokens: 0, cache_hit_bp: null,
      avg_latency_ms: null, avg_ttft_ms: null, ttft_samples: 0,
      share_bp: 10000, request_share_bp: 0, token_share_bp: 0 }],
  } }))
  await page.goto('/admin/stats?view=breakdown')
  const row = page.getByRole('row').filter({ hasText: 'refund-only' })
  await expect(row).toContainText('-US$0.001')
  await expect(row).toContainText('错误率 —')
  await expect(row).not.toContainText('错误率 0.0%')
  await expect(row).not.toContainText(' ms')
})

test('趋势金额纵轴和数据表保留 micro 精度，负退款不被零起点裁掉', async ({ page }) => {
  await prepare(page, () => report({ ...metrics, amount_micro: -1 }))
  await page.goto('/admin/stats')
  await expect(page.locator('svg.recharts-surface')).toContainText('-1E-6')
  await page.getByRole('button', { name: '数据表', exact: true }).click()
  await expect(page.getByRole('row').filter({ hasText: '2026-09-30' })).toContainText('-US$0.000001')
})
