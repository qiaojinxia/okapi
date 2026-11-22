import { expect, test } from '@playwright/test'
import type { Page } from '@playwright/test'
import { fileURLToPath } from 'node:url'

// 运维监控（IMPLEMENTATION §11.43）：实时概况、趋势、告警日志三个页签。接口全部打桩。
// 设 MONITOR_SHOTS=<目录> 时顺手截图（亮 / 暗 / 窄屏），供人工看布局。

type Json = Record<string, unknown>
const GB = 1024 ** 3

const overview = {
  collected_at: new Date().toISOString(),
  node: 'okapi-all-1',
  host: {
    cpus: 4, load: [1.2, 0.9, 0.7],
    memory: { total_bytes: 8 * GB, available_bytes: 1.5 * GB, swap_total_bytes: 2 * GB, swap_free_bytes: 1.8 * GB },
    disk: { total_bytes: 80 * GB, free_bytes: 6 * GB },
    uptime_secs: 3 * 86400 + 4 * 3600,
    process: { rss_bytes: 210 * 1024 ** 2, threads: 18, open_fds: 64 },
  },
  rates: { cpu_percent: 37.5, net_rx_bps: 1.2 * 1024 ** 2, net_tx_bps: 380 * 1024 },
  postgres: {
    ok: true, version: '16.4', uptime_secs: 86400, connections: 42, active: 3, idle_in_transaction: 1,
    max_connections: 100, longest_query_secs: 0.8, database_bytes: 1.4 * GB, cache_hit_ratio: 0.9987, deadlocks: 0,
    pool: { size: 12, idle: 9, max: 20 },
    tables: [{ name: 'usage_logs', bytes: 900 * 1024 ** 2, rows: 3_200_000 }, { name: 'billing_events', bytes: 210 * 1024 ** 2, rows: 820_000 }],
  },
  redis: {
    ok: true, version: '7.2.4', uptime_secs: 86400, clients: 37, used_memory: 380 * 1024 ** 2, used_memory_rss: 420 * 1024 ** 2,
    maxmemory: 512 * 1024 ** 2, fragmentation_ratio: 1.1, ops_per_sec: 812, hit_ratio: 0.94, evicted_keys: 0, keys: 18_233,
    rdb_last_bgsave_status: 'ok',
  },
  clickhouse: { ok: false, error: 'timeout' },
  nats: { ok: false, configured: false },
}

const now = Math.floor(Date.now() / 1000)
const samples = Array.from({ length: 30 }, (_, i) => ({
  t: now - (29 - i) * 60, node: 'okapi-all-1', cpu: 20 + (i % 7) * 5, load1: 1, mem_used: 6 * GB, mem_total: 8 * GB,
  disk_used: 74 * GB, disk_total: 80 * GB, net_rx: 1_000_000 + i * 10_000, net_tx: 300_000, pg_conns: 40 + (i % 3),
  pg_active: 2, redis_mem: 380 * 1024 ** 2, redis_clients: 37, redis_ops: 800, ch_mem: 1.1 * GB, ch_queries: 1,
  gw_rps: 14.2 + (i % 5), gw_5xx_rate: 0.4, gw_p95_ms: 185 + i * 2, ttft_p95_ms: 420 + i * 3,
  failovers_pm: i % 3, compactions_pm: i % 2, settle_backlog: 4 + (i % 4),
}))

const metricsResp = {
  collected_at: new Date().toISOString(),
  source: 'shared',
  node_count: 2,
  prometheus_path: '/metrics',
  summary: {
    gateway_rps: 18.4,
    console_rps: 1.2,
    gateway_in_flight: 11,
    console_in_flight: 2,
    gateway_requests_total: 42_500,
    gateway_5xx_total: 120,
    gateway_5xx_rate: 0.28,
    auth_cache_hits: 39_800,
    auth_cache_misses: 2_700,
    auth_cache_hit_ratio: 93.65,
    upstream_failovers: 37,
    failovers_per_min: 1.4,
    settle_backlog: 6,
    settle_in_flight: 2,
    settle_shedding: false,
    settle_batches_ok: 1_920,
    settle_batches_fallback: 3,
    settle_batch_size: { count: 1_923, avg: 12.4, p50: 10, p95: 32, p99: 48 },
    settle_batch_duration_ms: { count: 1_923, avg: 9.2, p50: 7.5, p95: 22.4, p99: 45 },
    cache_compactions: 19,
    compactions_per_min: 0.6,
    cache_compaction_saved_chars: 284_000,
    pool_quality_events_total: 8,
    routing_invalidations_local: 14,
    routing_invalidations_broadcast: 22,
    gateway_latency_ms: { count: 42_500, avg: 95.3, p50: 72.1, p95: 245.8, p99: 680.2 },
    upstream_ttft_ms: { count: 38_000, avg: 312.5, p50: 260.0, p95: 620.4, p99: 1420.0 },
    recent_gw_p95_ms: 210.5,
    recent_ttft_p95_ms: 540.0,
  },
  providers: [
    {
      provider: 'anthropic',
      total: 24_000,
      ok: 23_760,
      errors: 240,
      success_ratio: 99.0,
      outcomes: { ok: 23_760, http_429: 180, http_5xx: 60 },
      ttft_ms: { count: 23_760, avg: 340, p50: 280, p95: 650, p99: 1480 },
    },
  ],
  routes: [
    {
      role: 'gateway',
      route: '/v1/chat/completions',
      total: 35_000,
      s2xx: 34_400,
      s4xx: 500,
      s5xx: 100,
      latency_ms: { count: 35_000, avg: 102.4, p50: 78.2, p95: 256.0, p99: 710.5 },
    },
  ],
  pool_quality_events: [
    { pool: 'default', action: 'demote_error_rate', count: 6 },
    { pool: 'default', action: 'eject_error_rate', count: 2 },
  ],
  worker_jobs: [
    { job: 'reconcile', total: 120, outcomes: { ran: 118, skipped: 2 } },
  ],
  nodes: [
    {
      node: 'okapi-gw-1',
      role: 'gateway',
      ts: now,
      gateway_rps: 18.4,
      console_rps: 0,
      in_flight: 11,
      runtime: {
        pg_pool_size: 16,
        pg_pool_idle: 11,
        pg_pool_active: 5,
        pricebook_epoch: 19,
        settle_backlog: 6,
        settle_in_flight: 2,
        settle_shedding: false,
        caches: { models: 48, candidates: 92, channel_cost: 12, channel_control: 8, pool_policies: 5, settings: 24 },
      },
    },
  ],
}

const rawPrometheusText = [
  '# HELP okapi_http_in_flight Current in-flight HTTP requests.',
  '# TYPE okapi_http_in_flight gauge',
  'okapi_http_in_flight{role="gateway"} 11',
  'okapi_cache_compactions_total 19',
].join('\n')

const logs = {
  source: 'shared', total: 2, errors: 1,
  data: [
    { ts: new Date().toISOString(), level: 'ERROR', target: 'okapi::worker::chsink', message: 'clickhouse insert failed error=timeout', node: 'okapi-all-1', role: 'all' },
    { ts: new Date().toISOString(), level: 'WARN', target: 'okapi::gateway::key_health', message: 'upstream 429 channel=7', node: 'okapi-all-1', role: 'all' },
  ],
}

async function prepare(page: Page, seen: string[]) {
  await page.addInitScript(() => {
    localStorage.setItem('okapi.key', 'monitor-ui-fixture')
    localStorage.setItem('okapi.lang', 'zh-CN')
  })
  await page.route('**/*', async (route) => {
    const request = route.request()
    const url = new URL(request.url())
    if (request.isNavigationRequest()) return route.fulfill({
      path: fileURLToPath(new URL('../dist/index.html', import.meta.url)), contentType: 'text/html',
    })
    if (!/^\/(api|admin|auth)\//.test(url.pathname)) return route.continue()
    expect(request.method(), `unmocked write: ${url.pathname}`).toBe('GET')
    seen.push(url.pathname + url.search)
    if (url.pathname === '/admin/monitor/metrics/raw') {
      return route.fulfill({ contentType: 'text/plain; version=0.0.4; charset=utf-8', body: rawPrometheusText })
    }
    const json: Json = url.pathname === '/api/me'
      ? { user_id: 1, key_id: 1, group: 'default', balance_micro: 10_000_000, role: 100, permissions: ['*'] }
      : url.pathname === '/api/notice' ? { notice: null }
        : url.pathname === '/admin/monitor/overview' ? overview
          : url.pathname === '/admin/monitor/metrics' ? metricsResp
            : url.pathname === '/admin/monitor/history' ? { hours: 6, interval_secs: 60, data: samples }
              : url.pathname === '/admin/monitor/logs' ? (url.searchParams.get('level') === 'error'
                ? { ...logs, data: logs.data.filter((l) => l.level === 'ERROR') } : logs)
                : { data: [] }
    return route.fulfill({ json })
  })
  await page.goto('/admin/monitor')
}

async function shot(page: Page, name: string) {
  const dir = process.env.MONITOR_SHOTS
  if (!dir) return
  // 主题切换与页签下划线有过渡动画，等它走完再截
  await page.waitForTimeout(600)
  await page.screenshot({ path: `${dir}/${name}.png`, fullPage: true })
}

test('运维监控：侧栏直达；概况显示服务器占用与状态文字，中间件不可达 / 未配置分开标出', async ({ page }) => {
  const seen: string[] = []
  await prepare(page, seen)
  // 页内断言都限定在主内容区：侧栏与顶栏同名文字不该让用例误判
  const main = page.locator('#main-content')
  await expect(page.getByRole('heading', { name: '运维监控', level: 1 })).toBeVisible()
  await expect(page.getByRole('link', { name: '运维监控' })).toBeVisible()
  await expect(main.getByText('37.5%')).toBeVisible()
  // 磁盘 92.5%：告急，状态有文字不只靠颜色
  await expect(main.getByText('92.5%')).toBeVisible()
  await expect(main.getByText('告急').first()).toBeVisible()
  await expect(main.getByText('42 / 100')).toBeVisible()
  await expect(main.getByText('usage_logs')).toBeVisible()
  await expect(main.getByText('查询失败：timeout')).toBeVisible()
  await expect(main.getByText('未配置', { exact: true })).toBeVisible()
  await shot(page, 'overview-light')
  await page.emulateMedia({ colorScheme: 'dark' })
  await page.evaluate(() => document.documentElement.classList.add('dark'))
  await shot(page, 'overview-dark')
})

test('运维监控：运行时指标展示集群 Prometheus 汇总、调度降级与原文查看', async ({ page }) => {
  const seen: string[] = []
  await prepare(page, seen)
  const main = page.locator('#main-content')
  await page.getByRole('tab', { name: '运行时指标' }).click()
  await expect(main.getByText('18.4 req/s')).toBeVisible()
  await expect(main.getByText('93.7%')).toBeVisible()
  await expect(main.getByText('anthropic')).toBeVisible()
  await expect(main.getByText('/v1/chat/completions')).toBeVisible()
  await expect(main.getByText('demote_error_rate')).toBeVisible()
  await expect(main.getByText('okapi-gw-1')).toBeVisible()
  await page.getByRole('button', { name: '查看 Prometheus 原文' }).click()
  await expect(main.getByText('okapi_cache_compactions_total 19')).toBeVisible()
  await shot(page, 'metrics')
})

test('运维监控：趋势含宿主机与网关指标图；日志可切「仅错误」，带级别徽章与来源', async ({ page }) => {
  const seen: string[] = []
  await prepare(page, seen)
  await page.getByRole('tab', { name: '趋势' }).click()
  await expect(page.getByRole('group', { name: '资源占用率' })).toBeVisible()
  await expect(page.getByRole('group', { name: '中间件内存' })).toBeVisible()
  await expect(page.getByRole('group', { name: '网关吞吐与 5xx 率' })).toBeVisible()
  await expect(page.getByRole('group', { name: '网关与上游延迟（P95）' })).toBeVisible()
  await shot(page, 'trends')
  await page.getByRole('button', { name: '24 小时' }).click()
  await expect.poll(() => seen.some((u) => u === '/admin/monitor/history?hours=24')).toBe(true)

  await page.getByRole('tab', { name: '告警日志' }).click()
  const logs = page.locator('#main-content')
  await expect(logs.getByText('clickhouse insert failed error=timeout')).toBeVisible()
  await expect(logs.getByText('upstream 429 channel=7')).toBeVisible()
  await shot(page, 'logs')
  await page.getByRole('button', { name: '仅错误' }).click()
  await expect(logs.getByText('upstream 429 channel=7')).toHaveCount(0)
  await expect.poll(() => seen.some((u) => u.includes('level=error'))).toBe(true)
})

test('运维监控：窄屏不出横向滚动', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 })
  await prepare(page, [])
  await expect(page.locator('#main-content').getByText('37.5%')).toBeVisible()
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth)
  expect(overflow).toBeLessThanOrEqual(0)
  await shot(page, 'mobile')
})
