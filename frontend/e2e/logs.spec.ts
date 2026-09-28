import { test, expect } from '@playwright/test'
import type { Page } from '@playwright/test'
import { fileURLToPath } from 'node:url'
import { readFile } from 'node:fs/promises'
import { portalLogSearch } from '../src/features/logs/search'
import { billingLines, cacheRead, cacheReadShare, cacheWrite, netAmount } from '../src/features/logs/types'

const models = [
  { model: 'gpt-alpha', model_name: 'gpt-alpha', display_name: '通用助手', vendor: 'OpenAI' },
  { model: 'claude-beta', model_name: 'claude-beta', display_name: '写作助手', vendor: 'Anthropic' },
]
const log = {
  id: 20, request_id: 'req-20', upstream_request_id: 'upstream-20', model: 'gpt-alpha', log_type: 2, status: 20,
  user_id: 1, username: 'alice', api_key_id: 1, key_name: 'app-key', key_prefix: 'sk-prefix', channel_id: 1, channel_name: 'OpenAI Primary', channel_key_id: 2,
  provider: 'openai', client_type: 'sdk', client_ip: '203.0.113.1', node: 'edge-west', group: 'default',
  usage: { prompt_tokens: 1000, cached_tokens: 100, completion_tokens: 500, reasoning_tokens: 50 },
  amount_micro: 20000, original_amount_micro: 25000, discount_micro: 5000, upstream_cost_micro: 10000,
  pricing_snapshot: null, error_code: null, latency_ms: 800, ttft_ms: 100, is_stream: true,
  ts: '2026-09-26 12:00:00', created_at: '2026-09-26T12:00:00Z', retry_count: 0, failover_count: 0,
  sticky_layer: 0, upstream_status: 200, is_error: false, ratio_snapshot: '',
}

const detailedLog = {
  ...log, usage_details_recorded: true, endpoint: '/v1/responses', requested_model: 'model-alias', pool: 0,
  usage: { ...log.usage, cache_read_reported: true, cache_write_reported: true, cache_write_tokens: 100, audio_prompt_tokens: 0, image_prompt_tokens: 0, audio_completion_tokens: 0 },
  amount_micro: 4696, original_amount_micro: 5870, discount_micro: 1174,
  pricing_snapshot: { mode: 'ratio', epoch: 5, model_ratio: '1', completion_ratio: '4', cache_ratio: '0.1', cache_write_ratio: '1.25', final_unit_price_input_per_1m_usd: '1.6', group: 'default', group_ratio: '1', user_multiplier: '1', rules: [{ code: 'night', multiplier: '0.8' }] },
}

test('缓存输入占比：缺失和异常分母不推算，微小命中不舍入为零，部分命中不显示100%', () => {
  const share = (input: number, read: number) => cacheReadShare({ ...detailedLog, usage: { ...detailedLog.usage, prompt_tokens: input, cached_tokens: read } }, 'zh-CN')
  expect(share(1000, 800)).toBe('80%')
  expect(share(1000, 1000)).toBe('100%')
  expect(share(10_000_000, 1)).toBe('<0.1%')
  expect(share(10_000_000, 9_999_999)).toBe('>99.9%')
  for (const [input, read] of [[0, 1], [1000, 1001], [1000, 0], [NaN, 1], [1000, Infinity]]) expect(share(input, read)).toBeNull()
  expect(cacheReadShare({ ...log, usage: { ...log.usage, cached_tokens: 0 } }, 'en')).toBeNull()
})

for (const theme of ['light', 'dark']) {
  test(`Token合并列 ${theme}：命中、写入、真实零和缺失各自可辨，保持44px行高`, async ({ page }) => {
    await prepare(page)
    await page.addInitScript((value) => localStorage.setItem('okapi.theme', value), theme)
    await page.emulateMedia({ reducedMotion: 'reduce' })
    await page.setViewportSize({ width: 1440, height: 1000 })
    const records = [
      { ...detailedLog, usage: { ...detailedLog.usage, cached_tokens: 800 } },
      { ...detailedLog, usage: { ...detailedLog.usage, cached_tokens: 0, cache_write_tokens: 1000 } },
      { ...detailedLog, usage: { ...detailedLog.usage, cached_tokens: 0, cache_write_tokens: 0 } },
      { ...detailedLog, usage: { ...log.usage, cached_tokens: 0, cache_read_reported: false, cache_write_reported: false } },
      { ...log, usage: { ...log.usage, cached_tokens: 0 } },
      { ...detailedLog, usage: { ...detailedLog.usage, cached_tokens: 0, cache_read_reported: false } },
      { ...detailedLog, usage: { ...detailedLog.usage, prompt_tokens: 12_345_678, completion_tokens: 1_234_567, cached_tokens: 10_000_000, cache_write_tokens: 1_000_000 } },
      { ...detailedLog, usage: { ...detailedLog.usage, prompt_tokens: 10_000_000, cached_tokens: 1, cache_write_tokens: 0 } },
    ].map((row, i) => ({ ...row, id: 20 - i, request_id: `req-${20 - i}` }))
    await page.route('**/api/me/logs?*', (route) => route.fulfill({ json: { data: records, next_before: null } }))
    await page.route('**/api/me/logs/stat?*', (route) => route.fulfill({ json: { records: 8, settled: 8, failed: 0, refunded: 0, pending: 0, amount_micro: 32872, refunded_amount_micro: 0, prompt_tokens: 22351678, completion_tokens: 1238067, cached_tokens: 10000801, cache_read_samples: 5, avg_latency_ms: 800, latency_samples: 8, avg_ttft_ms: 100, ttft_samples: 8 } }))
    await page.goto('/portal/logs?scope=user')
    const table = page.getByRole('table', { name: '用量日志', exact: true })
    const cells = table.locator('[data-slot="log-token-usage"]')
    await expect(cells).toHaveCount(8)
    await expect(table.getByRole('columnheader')).toHaveCount(8)
    await expect(table.getByRole('columnheader', { name: '缓存读 / 写', exact: true })).toHaveCount(0)
    await expect(cells.nth(0)).toContainText('输入1,000')
    await expect(cells.nth(0)).toContainText('输出500')
    await expect(cells.nth(0)).toContainText('命中 80080%')
    await expect(cells.nth(0).getByLabel('缓存读取占输入 80%')).toBeVisible()
    await expect(cells.nth(0)).toContainText('写入 100')
    await expect(cells.nth(1)).toContainText('未命中')
    await expect(cells.nth(1)).toContainText('写入 1,000')
    await expect(cells.nth(2)).toContainText('未命中')
    await expect(cells.nth(2)).toContainText('写入 0')
    await expect(cells.nth(3)).toContainText('读取 未上报')
    await expect(cells.nth(3)).toContainText('写入 未上报')
    await expect(cells.nth(4)).toContainText('读取 未记录')
    await expect(cells.nth(4)).toContainText('写入 未记录')
    await expect(cells.nth(5)).toContainText('读取 未上报')
    await expect(cells.nth(5)).toContainText('写入 100')
    for (const index of [1, 2, 3, 4, 5]) await expect(cells.nth(index).locator('.lucide-zap')).toHaveCount(0)
    await expect(cells.nth(6)).toContainText('输入12,345,678')
    await expect(cells.nth(6)).toContainText('写入 1,000,000')
    await expect(cells.nth(7)).toContainText('<0.1%')
    for (const row of await table.locator('tbody tr').all()) expect(Math.abs(await row.evaluate((node) => node.offsetHeight) - 44)).toBeLessThanOrEqual(1)
    for (const cell of await cells.all()) expect((await cell.boundingBox())!.x).toBeCloseTo((await cells.first().boundingBox())!.x, 1)
    for (const tag of await cells.first().locator('[title^="缓存读取 800"], [title^="缓存写入 100"]').all()) {
      const contrast = await tag.evaluate((node) => {
        const canvas = document.createElement('canvas'), ctx = canvas.getContext('2d')!
        canvas.width = canvas.height = 1
        ctx.fillStyle = getComputedStyle(node.closest('[data-slot="table-frame"]')!).backgroundColor
        ctx.fillRect(0, 0, 1, 1)
        ctx.fillStyle = getComputedStyle(node).backgroundColor
        ctx.fillRect(0, 0, 1, 1)
        const bg = ctx.getImageData(0, 0, 1, 1).data
        ctx.fillStyle = getComputedStyle(node).color
        ctx.fillRect(0, 0, 1, 1)
        const fg = ctx.getImageData(0, 0, 1, 1).data
        const luminance = (rgba: Uint8ClampedArray) => [...rgba].slice(0, 3).map((v) => v / 255).map((v) => v <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4).reduce((sum, v, i) => sum + v * [0.2126, 0.7152, 0.0722][i], 0)
        const a = luminance(fg), b = luminance(bg)
        return (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05)
      })
      expect(contrast).toBeGreaterThanOrEqual(4.5)
    }
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    await page.screenshot({ path: `test-results/log-token-usage-${theme}.png`, animations: 'disabled' })
    const help = table.getByRole('button', { name: 'Token 用量说明' })
    await help.focus()
    await expect(page.getByRole('tooltip')).toContainText('缓存命中不等于费用减免')
    await expect(help).toHaveAccessibleDescription(/缓存已计入输入/)
    await page.keyboard.press('Escape')
    await expect(page.getByRole('tooltip')).toHaveCount(0)
    await page.getByRole('button', { name: '展开 req-20 的明细' }).focus()
    await page.keyboard.press('Enter')
    await expect(page.getByRole('dialog')).toContainText('缓存读取')
  })
}

for (const { width, language } of [
  { width: 1024, language: 'zh-CN' }, { width: 1440, language: 'en' },
  { width: 1920, language: 'zh-CN' }, { width: 2560, language: 'zh-CN' },
]) {
  test(`Token紧凑布局 ${width}px ${language}：宽屏不拉散，表头与内容对齐，缓存标签相邻`, async ({ page }) => {
    await prepare(page)
    await page.addInitScript((language) => {
      localStorage.setItem('okapi.lang', language)
      localStorage.setItem('okapi.theme', language === 'en' ? 'dark' : 'light')
    }, language)
    await page.emulateMedia({ reducedMotion: 'reduce' })
    await page.setViewportSize({ width, height: 1000 })
    const records = [
      { ...detailedLog, usage: { ...detailedLog.usage, prompt_tokens: 14, completion_tokens: 105, cached_tokens: 0, cache_write_tokens: 0 } },
      { ...detailedLog, usage: { ...detailedLog.usage, cached_tokens: 800 } },
      { ...detailedLog, status: 40, usage: { ...log.usage, prompt_tokens: 0, completion_tokens: 0, cached_tokens: 0, cache_read_reported: false, cache_write_reported: false } },
      { ...detailedLog, usage: { ...detailedLog.usage, prompt_tokens: 12_345_678, completion_tokens: 1_234_567, cached_tokens: 10_000_000, cache_write_tokens: 1_000_000 } },
    ].map((row, i) => ({ ...row, id: 20 - i, request_id: `req-${20 - i}` }))
    await page.route('**/api/me/logs?*', (route) => route.fulfill({ json: { data: records, next_before: null } }))
    await page.goto('/portal/logs?scope=user')
    const cells = page.locator('[data-slot="log-token-usage"]')
    await expect(cells).toHaveCount(4)
    const help = page.getByRole('button', { name: language === 'en' ? 'Token usage explained' : 'Token 用量说明', exact: true })
    const header = (await help.boundingBox())!
    const outputX: number[] = []
    for (const cell of await cells.all()) {
      const box = (await cell.boundingBox())!
      expect(box.width).toBeGreaterThanOrEqual(288)
      expect(box.width).toBeLessThanOrEqual(384)
      expect(Math.abs(box.x - header.x)).toBeLessThanOrEqual(1)
      const input = (await cell.locator('[data-slot="token-input"]').boundingBox())!
      const output = (await cell.locator('[data-slot="token-output"]').boundingBox())!
      expect(Math.abs(input.y - output.y)).toBeLessThanOrEqual(1)
      expect(output.x - input.x).toBeLessThan(200)
      outputX.push(output.x)
      const tags = cell.locator('[data-slot="token-cache"] > span')
      const read = (await tags.nth(0).boundingBox())!, write = (await tags.nth(1).boundingBox())!
      expect(write.x - read.x - read.width).toBeGreaterThanOrEqual(4)
      expect(write.x - read.x - read.width).toBeLessThanOrEqual(8)
      expect(write.x + write.width).toBeLessThanOrEqual(box.x + box.width + 1)
      expect(Math.abs((await cell.locator('xpath=ancestor::tr').boundingBox())!.height - 44)).toBeLessThanOrEqual(1)
    }
    expect(Math.max(...outputX) - Math.min(...outputX)).toBeLessThanOrEqual(1)
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    await page.screenshot({ path: `test-results/token-compact-${width}-${language}.png`, fullPage: true, animations: 'disabled' })
  })
}

test('日志费用解释：使用历史单价，缓存缺失不补零，退款不算消费，推理不另收费', () => {
  const lines = billingLines(detailedLog)
  expect(lines.map((line) => [line.name, line.quantity, line.amountMicro])).toEqual([
    ['normalInput',800,1280],['cacheRead',100,16],['cacheWrite',100,200],['textOutput',500,3200],
  ])
  expect(lines.reduce((sum,line) => sum + (line.amountMicro ?? 0),0)).toBe(4696)
  expect(cacheRead({ ...log, usage: { ...log.usage, cached_tokens: 0 } })).toBeNull()
  expect(cacheRead({ ...detailedLog, usage: { ...detailedLog.usage, cached_tokens: 0 } })).toBe(0)
  expect(cacheWrite(log)).toBeNull()
  expect(billingLines(log)).toEqual([])
  expect(netAmount({ ...detailedLog, status: 30 })).toBe(0)
  expect(portalLogSearch({ request_id: 'bad-id', api_key_id: -1 })).toMatchObject({ request_id: undefined, api_key_id: undefined })
})

test('日志汇总独立于已加载记录，退款/缺失/真实零明确，抽屉显示完整计价与请求依据', async ({ page }) => {
  await prepare(page)
  await page.setViewportSize({ width: 1440, height: 900 })
  await page.route('**/api/me/logs/stat?*', (route) => route.fulfill({ json: { records: 77, settled: 73, failed: 3, refunded: 1, pending: 0, amount_micro: 12000000, refunded_amount_micro: 20000, prompt_tokens: 90000, completion_tokens: 12000, cached_tokens: 2000, cache_read_samples: 74, avg_latency_ms: 2400, latency_samples: 73, avg_ttft_ms: 120, ttft_samples: 70 } }))
  await page.route('**/api/me/logs?*', (route) => route.fulfill({ json: { data: [detailedLog, { ...detailedLog, id: 19, request_id: 'req-19', status: 30 }, { ...log, id: 18, request_id: 'req-18', usage: { ...log.usage, cached_tokens: 0 }, is_stream: false }], next_before: null } }))
  await page.goto('/portal/logs?scope=user')
  const summary = page.getByRole('region', { name: '筛选范围汇总' })
  await expect(summary).toContainText('77')
  await expect(summary).toContainText('120 ms')
  await expect(page.getByText('已加载 3 / 共 77 条')).toBeVisible()
  await expect(page.getByRole('table').getByText('已退款')).toBeVisible()
  await expect(page.getByRole('table').getByText('非流式')).toBeVisible()
  await expect(page.getByRole('button', { name: '导出已加载 CSV' })).toHaveAttribute('title', '仅导出已加载的 3 条，不是全部筛选结果。')
  await page.screenshot({ path: 'test-results/usage-logs-summary-desktop.png', animations: 'disabled' })
  await page.getByRole('button', { name: '展开 req-20 的明细' }).click()
  const detail = page.getByRole('dialog', { name: '请求与账单详情' })
  await expect(detail).toContainText('/v1/responses')
  await expect(detail).toContainText('model-alias')
  await expect(detail.getByRole('table', { name: '快照计费分项' })).toContainText('缓存写入')
  await expect(detail).toContainText('不再重复乘倍率')
  await expect(detail).toContainText('night ×0.8')
  await page.screenshot({ path: 'test-results/usage-logs-detail-desktop.png', animations: 'disabled' })
  await detail.getByRole('table', { name: '快照计费分项' }).scrollIntoViewIfNeeded()
  await page.screenshot({ path: 'test-results/usage-logs-billing-desktop.png', animations: 'disabled' })
  await page.keyboard.press('Escape')
  await page.getByRole('button', { name: '展开 req-18 的明细' }).click()
  await expect(detail).toContainText('未记录')
  await expect(detail).toContainText('不适用')
  await expect(detail.getByRole('table')).toHaveCount(0)
})

test('用户日志按自有密钥和请求ID定位，汇总条件同步；错误ID不发查询，清除后恢复', async ({ page }) => {
  const queries = await prepare(page)
  await page.route('**/api/me/keys', (route) => route.fulfill({ json: { data: [{ id: 42, name: 'production-key', key_prefix: 'sk-public' }] } }))
  await page.goto('/portal/logs?scope=user')
  const key = page.getByRole('combobox', { name: '筛选密钥' })
  await key.fill('production')
  await page.getByRole('option', { name: /production-key/ }).click()
  await expect(page).toHaveURL(/api_key_id=42/)
  await expect.poll(() => queries.filter((q) => q.pathname === '/api/me/logs/stat').at(-1)?.searchParams.get('api_key_id')).toBe('42')
  const id = page.getByRole('textbox', { name: '请求 ID', exact: true })
  const count = queries.filter((q) => q.pathname === '/api/me/logs').length
  await id.fill('bad-id')
  await id.press('Enter')
  await expect(page.getByRole('button', { name: '搜索', exact: true })).toBeDisabled()
  expect(queries.filter((q) => q.pathname === '/api/me/logs')).toHaveLength(count)
  const requestId = '11111111-2222-3333-4444-555555555555'
  await id.fill(requestId)
  await id.press('Enter')
  await expect.poll(() => queries.filter((q) => q.pathname === '/api/me/logs/stat').at(-1)?.searchParams.get('request_id')).toBe(requestId)
  await page.reload()
  await expect(id).toHaveValue(requestId)
  await page.getByRole('button', { name: '清除定位条件' }).click()
  await expect(page).not.toHaveURL(/api_key_id|request_id/)
  expect(queries.some((q) => q.pathname === '/admin/keys')).toBe(false)
})

test('汇总错误不伪装零消费或阻塞明细；空TTFT不显示0ms', async ({ page }) => {
  await prepare(page)
  await page.route('**/api/me/logs/stat?*', (route) => route.fulfill({ status: 500, json: { error: { code: 'internal_error' } } }))
  await page.goto('/portal/logs')
  await expect(page.getByRole('region', { name: '筛选范围汇总' })).toContainText('汇总暂不可用')
  await expect(page.getByRole('table').locator('tbody tr')).toHaveCount(1)
  await expect(page.getByRole('region', { name: '筛选范围汇总' })).not.toContainText('0 ms')
  await expect(page.getByRole('region', { name: '筛选范围汇总' })).not.toContainText('$0.00')
})

for (const width of [390, 1440]) {
  test(`日志紧凑表 ${width}px：管理端和门户字级、表头、行高一致，分页位于表格下方`, async ({ page }) => {
    await prepare(page)
    await page.emulateMedia({ reducedMotion: 'reduce' })
    await page.setViewportSize({ width, height: 1000 })
    for (const path of ['/admin/logs', '/portal/logs']) {
      await page.goto(path)
      const table = page.getByRole('table')
      await expect(table.locator('tbody tr')).toHaveCount(1)
      await expect(table.locator('td').nth(1)).toHaveCSS('font-size', '13px')
      expect(Math.abs((await table.locator('thead tr').boundingBox())!.height - 40)).toBeLessThanOrEqual(1)
      expect(Math.abs((await table.locator('tbody tr').boundingBox())!.height - (width < 768 ? 56 : 44))).toBeLessThanOrEqual(1)
      if (path === '/admin/logs') {
        const frame = (await page.locator('[data-slot="table-frame"]').boundingBox())!
        const footer = page.getByRole('navigation', { name: '分页' })
        expect((await footer.boundingBox())!.y).toBeGreaterThanOrEqual(frame.y + frame.height)
        await footer.scrollIntoViewIfNeeded()
        await expect(footer).toBeInViewport({ ratio: 1 })
      }
      expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    }
  })
}
async function prepare(page: Page) {
  const queries: URL[] = []
  await page.addInitScript(() => { localStorage.setItem('okapi.key', 'logs-fixture'); localStorage.setItem('okapi.lang', 'zh-CN') })
  await page.route('**/*', (route) => {
    const request = route.request(), url = new URL(request.url())
    if (request.isNavigationRequest()) return route.fulfill({ path: fileURLToPath(new URL('../dist/index.html', import.meta.url)), contentType: 'text/html' })
    if (!/^\/(api|admin|auth)\//.test(url.pathname)) return route.continue()
    expect(request.method()).toBe('GET')
    queries.push(url)
    const responses: Record<string, unknown> = {
      '/api/me': { user_id: 1, key_id: 1, role: 100, permissions: ['*'], balance_micro: 20000000, group: 'default' },
      '/api/notice': { notice: null }, '/admin/models': { data: models }, '/api/pricing': { models, groups: [] },
      '/api/me/logs': { data: [log], scope: url.searchParams.get('scope'), next_before: null },
      '/admin/logs': { data: [log] },
      '/admin/logs/stat': { requests: 12, errors: 1, error_rate_bp: 833, tokens: 18000, amount_micro: 240000, discount_micro: 10000, users: 2, cached_tokens: 1000, cache_hit_bp: 1000, rpm: 2, tpm: 400, rate_source: 'clickhouse' },
    }
    return route.fulfill({ json: responses[url.pathname] ?? { data: [] } })
  })
  return queries
}

test('门户日志：承接全账户范围，联想选择准确模型，刷新与后退恢复已应用条件', async ({ page }) => {
  const queries = await prepare(page)
  await page.goto('/portal?scope=user')
  await page.getByRole('link', { name: '调用明细' }).click()
  await expect(page.getByRole('button', { name: '展开 req-20 的明细', exact: true })).toBeVisible()
  await expect(page).toHaveURL(/scope=user/)
  await expect(page.getByRole('button', { name: '全账户', exact: true })).toHaveAttribute('aria-pressed', 'true')
  const model = page.getByRole('combobox', { name: '模型', exact: true })
  const count = queries.filter((url) => url.pathname === '/api/me/logs').length
  await model.fill('写作 anthropic')
  await expect(page.getByRole('option')).toHaveCount(1)
  expect(queries.filter((url) => url.pathname === '/api/me/logs')).toHaveLength(count)
  await model.press('ArrowDown')
  await model.press('Enter')
  await expect(model).toHaveValue('claude-beta')
  await expect(page).toHaveURL(/model=claude-beta/)
  await expect.poll(() => queries.filter((url) => url.pathname === '/api/me/logs').at(-1)?.searchParams.get('model')).toBe('claude-beta')
  await page.getByRole('switch', { name: '只看失败' }).click()
  await expect(page).toHaveURL(/errors_only=true/)
  await page.reload()
  await expect(model).toHaveValue('claude-beta')
  await expect(page.getByRole('switch', { name: '只看失败' })).toBeChecked()
  await page.goBack()
  await expect(page.getByRole('switch', { name: '只看失败' })).not.toBeChecked()
  await expect(model).toHaveValue('claude-beta')
  expect(queries.some((url) => url.pathname === '/admin/models')).toBe(false)
})

test('门户日志：目录失败仍可手输历史模型，中文确认不查询，提交后保存准确名称', async ({ page }) => {
  const queries = await prepare(page)
  await page.route('**/api/pricing', (route) => route.fulfill({ status: 500, json: { error: { code: 'internal_error' } } }))
  await page.goto('/portal/logs')
  await expect(page.getByRole('button', { name: '展开 req-20 的明细', exact: true })).toBeVisible()
  const model = page.getByRole('combobox', { name: '模型', exact: true })
  await model.fill('retired-model')
  const before = queries.filter((url) => url.pathname === '/api/me/logs').length
  await model.dispatchEvent('keydown', { key: 'Enter', code: 'Enter', isComposing: true, bubbles: true })
  expect(queries.filter((url) => url.pathname === '/api/me/logs')).toHaveLength(before)
  await model.press('Enter')
  await expect(page).toHaveURL(/model=retired-model/)
  await page.getByRole('button', { name: '清空', exact: true }).click()
  await expect(model).toBeFocused()
  await model.press('Enter')
  await expect(page).not.toHaveURL(/model=/)
})

test('门户日志日期：应用、清除、刷新和后退保留其他条件，无效日期不能提交', async ({ page }) => {
  const queries = await prepare(page)
  await page.goto('/portal/logs?scope=user&model=gpt-alpha&start_date=2024-11-03&end_date=2024-11-03&timezone=America%2FLos_Angeles')
  const period = page.getByRole('region', { name: '统计时段', exact: true })
  await expect(period).toContainText('2024-11-03 — 2024-11-03')
  await period.locator('summary').click()
  await expect(page.getByLabel('开始日期')).toHaveValue('2024-11-03')
  const count = queries.filter((url) => url.pathname === '/api/me/logs').length
  await page.getByLabel('开始日期').fill('2024-11-01')
  await page.getByLabel('结束日期').fill('2024-10-31')
  await expect(page.getByRole('button', { name: '应用日期', exact: true })).toBeDisabled()
  expect(queries.filter((url) => url.pathname === '/api/me/logs')).toHaveLength(count)
  await page.getByLabel('结束日期').fill('2024-11-03')
  await page.getByRole('button', { name: '应用日期', exact: true }).click()
  await expect.poll(() => queries.filter((url) => url.pathname === '/api/me/logs').at(-1)?.searchParams.get('start_date')).toBe('2024-11-01')
  await page.reload()
  await expect(period).toContainText('2024-11-01 — 2024-11-03')
  await page.getByRole('switch', { name: '只看失败' }).click()
  await page.getByRole('button', { name: '清除日期', exact: true }).click()
  await expect(period).toContainText('全部日期')
  await expect(page).toHaveURL('/portal/logs?scope=user&model=gpt-alpha&errors_only=true')
  await expect.poll(() => queries.filter((url) => url.pathname === '/api/me/logs').at(-1)?.searchParams.has('start_date')).toBe(false)
  await page.goBack()
  await expect(period).toContainText('2024-11-01 — 2024-11-03')
  await expect(page.getByRole('switch', { name: '只看失败' })).toBeChecked()
  await expect(page.getByRole('combobox', { name: '模型', exact: true })).toHaveValue('gpt-alpha')
  await expect(page).toHaveURL(/timezone=America%2FLos_Angeles/)
})

test('门户日志按日期翻页保留条件，重新选日期从第一页开始且收起旧明细', async ({ page }) => {
  const queries = await prepare(page)
  await page.route('**/api/me/logs?*', (route) => {
    const url = new URL(route.request().url())
    queries.push(url)
    const first = !url.searchParams.has('before')
    return route.fulfill({ json: { scope: 'user', data: first ? Array.from({ length: 50 }, (_, i) => ({ ...log, id: 100 - i, request_id: `req-${100 - i}` })) : [{ ...log, id: 50, request_id: 'req-50' }], next_before: first ? 51 : null } })
  })
  await page.goto('/portal/logs?scope=user&model=gpt-alpha&errors_only=true&start_date=2024-11-03&end_date=2024-11-03&timezone=America%2FLos_Angeles')
  await page.getByRole('button', { name: '加载更多', exact: true }).click()
  await expect(page.getByRole('button', { name: '展开 req-50 的明细', exact: true })).toBeAttached()
  expect(Object.fromEntries(queries.filter((url) => url.pathname === '/api/me/logs').at(-1)!.searchParams)).toMatchObject({ before: '51', model: 'gpt-alpha', errors_only: 'true', scope: 'user', start_date: '2024-11-03', end_date: '2024-11-03', timezone: 'America/Los_Angeles' })
  const first = page.getByRole('button', { name: '展开 req-100 的明细', exact: true })
  await first.click()
  await expect(page.getByRole('button', { name: '收起 req-100 的明细', exact: true })).toHaveAttribute('aria-expanded', 'true')
  await expect(page.getByRole('dialog', { name: '请求与账单详情' })).toBeVisible()
  await page.keyboard.press('Escape')
  await page.getByRole('region', { name: '统计时段', exact: true }).locator('summary').click()
  await page.getByLabel('开始日期').fill('2024-11-02')
  await page.getByRole('button', { name: '应用日期', exact: true }).click()
  await expect.poll(() => queries.filter((url) => url.pathname === '/api/me/logs').at(-1)?.searchParams.get('start_date')).toBe('2024-11-02')
  expect(queries.filter((url) => url.pathname === '/api/me/logs').at(-1)!.searchParams.has('before')).toBe(false)
  await expect(first).toHaveAttribute('aria-expanded', 'false')
  await expect(page.getByRole('button', { name: '展开 req-50 的明细', exact: true })).toHaveCount(0)
})

test('门户日志日期地址只接受成对的真实日期和最多366天，保留模型与失败条件', () => {
  for (const range of [
    { start_date: '2024-02-29' }, { start_date: '2023-02-29', end_date: '2023-03-01' },
    { start_date: '2024-03-02', end_date: '2024-03-01' }, { start_date: '2023-01-01', end_date: '2024-01-02' },
    { start_date: '2150-01-01', end_date: '2150-01-01' },
  ]) {
    const result = portalLogSearch({ ...range, timezone: 'UTC', model: 'historical-model', errors_only: 'true' })
    expect(result).toMatchObject({ model: 'historical-model', errors_only: true })
    expect(result.start_date).toBeUndefined()
    expect(result.end_date).toBeUndefined()
    expect(result.timezone).toBeUndefined()
  }
  expect(portalLogSearch({ start_date: '2024-01-01', end_date: '2024-12-31', timezone: 'Asia/Shanghai' })).toMatchObject({ start_date: '2024-01-01', end_date: '2024-12-31', timezone: 'Asia/Shanghai' })
})

for (const width of [320, 390]) {
  test(`门户日期筛选 ${width}px：长模型、日期面板和时区说明不溢出`, async ({ page }) => {
    await prepare(page)
    await page.setViewportSize({ width, height: 800 })
    if (width === 390) await page.addInitScript(() => { localStorage.setItem('okapi.lang', 'en'); localStorage.setItem('okapi.theme', 'dark') })
    await page.goto('/portal/logs?scope=user&model=long-historical-model-for-a-specific-workload&start_date=2024-11-03&end_date=2024-11-03&timezone=America%2FLos_Angeles')
    const period = page.getByRole('region', { name: width === 320 ? '统计时段' : 'Time period', exact: true })
    await period.locator('summary').click()
    await expect(period.locator('input[type=date]')).toHaveCount(2)
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    for (const input of await period.locator('input[type=date]').all()) {
      expect(await input.evaluate((node) => node.getBoundingClientRect().right <= innerWidth)).toBe(true)
    }
    await page.screenshot({ path: `test-results/portal-log-range-${width}.png`, fullPage: true, animations: 'disabled' })
  })
}

for (const width of [390, 1024, 1440, 1920]) {
  test(`日志筛选排版 ${width}px：实体字段限宽并对齐，UUID 保留长输入空间`, async ({ page }) => {
    await prepare(page)
    await page.emulateMedia({ reducedMotion: 'reduce' })
    await page.setViewportSize({ width, height: 1000 })
    await page.goto('/admin/logs')
    await page.getByText('更多筛选', { exact: true }).click()
    await page.getByText('自定义时间', { exact: true }).click()
    const entityBoxes = await Promise.all(['user_id', 'api_key_id', 'channel_id'].map(async (name) => (await page.locator(`#lf-${name}`).boundingBox())!))
    if (width >= 1024) {
      for (const box of entityBoxes) { expect(box.width).toBeLessThanOrEqual(320); expect(box.height).toBe(36) }
      expect(Math.max(...entityBoxes.map((b) => b.y)) - Math.min(...entityBoxes.map((b) => b.y))).toBeLessThanOrEqual(1)
      expect((await page.locator('#lf-model').boundingBox())!.width).toBeLessThanOrEqual(384)
      const error = (await page.locator('#lf-error_code').boundingBox())!, request = (await page.locator('#lf-request_id').boundingBox())!
      expect(error.width).toBeLessThanOrEqual(320)
      expect(request.width).toBeGreaterThanOrEqual(error.width)
      expect(request.width).toBeLessThanOrEqual(512)
      expect(Math.abs(error.y - request.y)).toBeLessThanOrEqual(1)
      const from = (await page.locator('#logs-from').boundingBox())!, to = (await page.locator('#logs-to').boundingBox())!
      expect(from.width).toBeLessThanOrEqual(320)
      expect(to.width).toBeLessThanOrEqual(320)
      expect(Math.abs(from.y - to.y)).toBeLessThanOrEqual(1)
    }
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    await page.screenshot({ path: `test-results/admin-log-filters-${width}.png`, fullPage: true, animations: 'disabled' })
    await page.goto('/portal/logs?scope=user')
    const model = page.getByRole('combobox', { name: '模型', exact: true })
    const request = page.getByRole('textbox', { name: '请求 ID', exact: true })
    await expect(model).toBeVisible()
    await expect(request).toBeVisible()
    if (width >= 1024) {
      expect((await model.boundingBox())!.width).toBe(320)
      expect((await request.boundingBox())!.width).toBe(384)
    }
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    await page.screenshot({ path: `test-results/portal-log-filters-${width}.png`, fullPage: true, animations: 'disabled' })
  })
}

test('管理日志：高级筛选按需展开，候选只填草稿，无效ID不触发查询', async ({ page }) => {
  const queries = await prepare(page)
  await page.goto('/admin/logs')
  await expect(page.getByRole('button', { name: '展开 req-20 的明细', exact: true })).toBeVisible()
  await expect(page.locator('#lf-user_id')).not.toBeVisible()
  await expect(page.getByLabel('起始时间')).not.toBeVisible()
  const model = page.getByRole('combobox', { name: '模型', exact: true })
  const count = queries.filter((url) => url.pathname === '/admin/logs').length
  await model.fill('写作 anthropic')
  await page.getByRole('listbox').getByRole('option', { name: /claude-beta/ }).click()
  await expect(model).toHaveValue('claude-beta')
  expect(queries.filter((url) => url.pathname === '/admin/logs')).toHaveLength(count)
  await page.getByText('更多筛选', { exact: true }).click()
  await page.locator('#lf-user_id').fill('-1')
  await expect(page.getByRole('button', { name: '搜索', exact: true })).toBeDisabled()
  await expect(page.locator('#lf-user_id')).toHaveAccessibleDescription(/^请输入大于 0 的整数 ID。/)
  await page.locator('#lf-user_id').press('Enter')
  expect(queries.filter((url) => url.pathname === '/admin/logs')).toHaveLength(count)
  await page.locator('#lf-user_id').fill('42')
  await page.getByRole('button', { name: '搜索', exact: true }).click()
  await expect(page).toHaveURL(/model=claude-beta/)
  await expect(page).toHaveURL(/user_id=42/)
  await page.reload()
  await expect(page.locator('#lf-user_id')).toBeVisible()
  await expect(page.locator('#lf-user_id')).toHaveValue('42')
})

test('日志展开显示调用对象和密钥前缀，CSV 附带名称且保留原有列顺序', async ({ page }) => {
  await prepare(page)
  await page.goto('/admin/logs')
  await page.getByRole('button', { name: '展开 req-20 的明细', exact: true }).click()
  const identities = page.getByLabel('调用对象', { exact: true })
  await expect(identities).toContainText('alice')
  await expect(identities).toContainText('app-key')
  await expect(identities).toContainText('sk-prefix…')
  await expect(identities).toContainText('OpenAI Primary')
  await expect(identities).not.toContainText('#1')
  const [download] = await Promise.all([page.waitForEvent('download'), page.getByRole('button', { name: '导出 CSV', exact: true }).click()])
  const csv = await readFile((await download.path())!, 'utf8')
  const lines = csv.trim().split('\n')
  expect(lines[0]).toMatch(/upstream_request_id,node,key_name,key_prefix$/)
  expect(lines[1]).toContain(',edge-west,app-key,sk-prefix')
})

test('旧日志未返回密钥名称时明确降级，不把未知对象显示成其他记录的名称', async ({ page }) => {
  await prepare(page)
  await page.route('**/admin/logs?*', (route) => route.request().isNavigationRequest() ? route.fallback() : route.fulfill({ json: { data: [{ ...log, key_name: undefined, key_prefix: undefined, username: '', channel_name: '', channel_id: 0 }] } }))
  await page.goto('/admin/logs?api_key_id=1')
  await expect(page.locator('#lf-api_key_id')).toHaveValue('1')
  await page.getByRole('button', { name: '展开 req-20 的明细', exact: true }).click()
  const identities = page.getByLabel('调用对象', { exact: true })
  await expect(identities.getByText('名称不可用', { exact: true })).toHaveCount(2)
  await expect(identities).toContainText('未分配渠道')
  await expect(identities).not.toContainText('undefined')
})

for (const width of [320, 390, 1280]) {
  test(`日志调用对象 ${width}px：展开信息跟随可见表宽，横向看列不会把明细移走`, async ({ page }) => {
    await prepare(page)
    await page.setViewportSize({ width, height: 800 })
    await page.goto('/admin/logs')
    await page.getByRole('button', { name: '展开 req-20 的明细', exact: true }).click()
    const identities = page.getByLabel('调用对象', { exact: true })
    const wrapper = page.getByRole('table').locator('..')
    await identities.scrollIntoViewIfNeeded()
    const viewport = await wrapper.boundingBox()
    const objects = await identities.boundingBox()
    expect(objects!.width).toBeLessThanOrEqual(viewport!.width)
    expect(objects!.x).toBeGreaterThanOrEqual(viewport!.x)
    expect(objects!.x + objects!.width).toBeLessThanOrEqual(viewport!.x + viewport!.width)
    await wrapper.evaluate((node) => { node.scrollLeft = node.scrollWidth })
    const moved = await identities.boundingBox()
    expect(moved!.x).toBeGreaterThanOrEqual(viewport!.x)
    expect(moved!.x + moved!.width).toBeLessThanOrEqual(viewport!.x + viewport!.width)
    expect(await identities.evaluate((node) => node.scrollWidth <= node.clientWidth)).toBe(true)
    await page.screenshot({ path: `test-results/log-identities-${width}.png`, animations: 'disabled' })
  })
}

for (const path of ['/portal/logs', '/admin/logs']) {
  test(`${path} 明细可键盘展开和收起，不因按钮冒泡切换两次`, async ({ page }) => {
    await prepare(page)
    await page.goto(path)
    const expand = page.getByRole('button', { name: '展开 req-20 的明细', exact: true })
    await expand.focus()
    await expand.press('Enter')
    const collapse = page.getByRole('button', { name: '收起 req-20 的明细', exact: true })
    if (path === '/admin/logs') await expect(collapse).toBeFocused()
    const controls = await collapse.getAttribute('aria-controls')
    await expect(page.locator(`[id="${controls}"]`)).toBeVisible()
    await expect(page.getByText('req-20', { exact: true })).toBeVisible()
    if (path === '/portal/logs') {
      await expect(page.getByRole('dialog', { name: '请求与账单详情' })).toBeVisible()
      await page.keyboard.press('Escape')
      await expect(expand).toBeFocused()
    } else await collapse.press('Space')
    await expect(expand).toHaveAttribute('aria-expanded', 'false')
    await expect(page.getByText('req-20', { exact: true })).toHaveCount(0)
  })
}

for (const width of [320, 390, 1280]) {
  test(`日志 ${width}px：日期展开不撑宽，倒序日期不提交，回到预设收起日期`, async ({ page }) => {
    const queries = await prepare(page)
    await page.setViewportSize({ width, height: 800 })
    await page.goto('/admin/logs')
    await expect(page.getByText('alice', { exact: true })).toBeVisible()
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    await page.screenshot({ path: `test-results/admin-logs-${width}.png`, fullPage: true, animations: 'disabled' })
    await page.getByText('自定义时间', { exact: true }).click()
    await page.getByLabel('起始时间').fill('2026-09-20T10:00')
    await page.getByLabel('结束时间').fill('2026-09-19T10:00')
    await expect(page.getByRole('button', { name: '应用区间' })).toBeDisabled()
    const count = queries.filter((url) => url.pathname === '/admin/logs').length
    await page.getByLabel('结束时间').press('Enter')
    expect(queries.filter((url) => url.pathname === '/admin/logs')).toHaveLength(count)
    await page.getByLabel('结束时间').fill('2026-09-21T10:00')
    await expect(page.getByRole('button', { name: '应用区间' })).toBeEnabled()
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    await page.evaluate(() => window.scrollTo(0, 0))
    await page.screenshot({ path: `test-results/logs-dates-${width}.png`, fullPage: true, animations: 'disabled' })
    await page.getByRole('button', { name: '应用区间' }).click()
    await expect(page).toHaveURL(/from=/)
    await expect(page).toHaveURL(/to=/)
    await page.getByRole('button', { name: '7 天', exact: true }).click()
    await expect(page).toHaveURL(/hours=168/)
    await expect(page).not.toHaveURL(/from=/)
    await expect(page.getByLabel('起始时间')).not.toBeVisible()
    await page.goto('/portal/logs')
    await expect(page.getByRole('combobox', { name: '模型' })).toBeVisible()
    if (width < 640) expect((await page.getByRole('combobox', { name: '模型' }).boundingBox())!.width).toBeGreaterThan(width - 70)
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    await page.screenshot({ path: `test-results/portal-logs-${width}.png`, fullPage: true, animations: 'disabled' })
  })
}
