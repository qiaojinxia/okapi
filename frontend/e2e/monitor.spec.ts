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
}))

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
    const json: Json = url.pathname === '/api/me'
      ? { user_id: 1, key_id: 1, group: 'default', balance_micro: 10_000_000, role: 100, permissions: ['*'] }
      : url.pathname === '/api/notice' ? { notice: null }
        : url.pathname === '/admin/monitor/overview' ? overview
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
  await expect(page.getByRole('heading', { name: '运维监控', level: 1 })).toBeVisible()
  await expect(page.getByRole('link', { name: '运维监控' })).toBeVisible()
  await expect(page.getByText('37.5%')).toBeVisible()
  // 磁盘 92.5%：告急，状态有文字不只靠颜色
  await expect(page.getByText('92.5%')).toBeVisible()
  await expect(page.getByText('告急').first()).toBeVisible()
  await expect(page.getByText('42 / 100')).toBeVisible()
  await expect(page.getByText('usage_logs')).toBeVisible()
  await expect(page.getByText('查询失败：timeout')).toBeVisible()
  await expect(page.getByText('未配置', { exact: true })).toBeVisible()
  await shot(page, 'overview-light')
  await page.emulateMedia({ colorScheme: 'dark' })
  await page.evaluate(() => document.documentElement.classList.add('dark'))
  await shot(page, 'overview-dark')
})

test('运维监控：趋势四张图带表格视图；日志可切「仅错误」，带级别徽章与来源', async ({ page }) => {
  const seen: string[] = []
  await prepare(page, seen)
  await page.getByRole('tab', { name: '趋势' }).click()
  await expect(page.getByRole('group', { name: '资源占用率' })).toBeVisible()
  await expect(page.getByRole('group', { name: '中间件内存' })).toBeVisible()
  await shot(page, 'trends')
  await page.getByRole('button', { name: '24 小时' }).click()
  await expect.poll(() => seen.some((u) => u === '/admin/monitor/history?hours=24')).toBe(true)

  await page.getByRole('tab', { name: '告警日志' }).click()
  await expect(page.getByText('clickhouse insert failed error=timeout')).toBeVisible()
  await expect(page.getByText('upstream 429 channel=7')).toBeVisible()
  await shot(page, 'logs')
  await page.getByRole('button', { name: '仅错误' }).click()
  await expect(page.getByText('upstream 429 channel=7')).toHaveCount(0)
  await expect.poll(() => seen.some((u) => u.includes('level=error'))).toBe(true)
})

test('运维监控：窄屏不出横向滚动', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 })
  await prepare(page, [])
  await expect(page.getByText('37.5%')).toBeVisible()
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth)
  expect(overflow).toBeLessThanOrEqual(0)
  await shot(page, 'mobile')
})
