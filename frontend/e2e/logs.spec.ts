import { test, expect } from '@playwright/test'
import type { Page } from '@playwright/test'
import { fileURLToPath } from 'node:url'
import { readFile } from 'node:fs/promises'
import { portalLogSearch } from '../src/features/logs/search'
import { billingLines, cacheRead, cacheReadShare, cacheWrite, netAmount } from '../src/features/logs/types'
import { formatOutputRate, outputRates } from '../src/features/logs/performance'

const models = [
  { model: 'gpt-alpha', model_name: 'gpt-alpha', display_name: '通用助手', vendor: 'OpenAI' },
  { model: 'claude-beta', model_name: 'claude-beta', display_name: '写作助手', vendor: 'Anthropic' },
]
const log = {
  id: 20, request_id: 'req-20', upstream_request_id: 'upstream-20', model: 'gpt-alpha', log_type: 2, status: 20,
  user_id: 1, username: 'alice', api_key_id: 1, key_name: 'app-key', key_prefix: 'sk-prefix', channel_id: 1, channel_name: 'OpenAI Primary', channel_key_id: 2,
  provider: 'openai', client_type: 'sdk', client_ip: '203.0.113.1', node: 'edge-west', group: 'default',
  usage: { prompt_tokens: 1000, cached_tokens: 100, completion_tokens: 500, reasoning_tokens: 50, input_unit: 'tokens' },
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

const cacheSubsetLabels = ['其中 5 分钟写入', '其中 1 小时写入', '缓存读取 · 音频', '缓存读取 · 图片', '缓存写入 · 音频', '缓存写入 · 图片']

for (const path of ['/admin/logs', '/portal/logs?scope=user']) {
  for (const theme of ['light', 'dark']) {
    test(`日志详情字体 ${path} ${theme}：字段和ID沿用正文字体，字级统一，复制不截断`, async ({ page }) => {
      await prepare(page)
      await page.addInitScript((theme) => {
        localStorage.setItem('okapi.theme', theme)
        Object.defineProperty(navigator, 'clipboard', { value: { writeText: async (value: string) => {
          (window as Window & { logDetailCopied?: string }).logDetailCopied = value
        } }, configurable: true })
      }, theme)
      await page.emulateMedia({ reducedMotion: 'reduce' })
      await page.setViewportSize({ width: 1440, height: 1000 })
      const requestId = 'ccbd2d96-cdde-4e83-8c24-75a7945efef6'
      const row = { ...detailedLog, request_id: requestId, upstream_model: 'gpt-alpha', upstream_endpoint: '/v1/responses',
        ratio_snapshot: JSON.stringify(detailedLog.pricing_snapshot) }
      await page.route('**/api/me/logs?*', route => route.fulfill({ json: { data: [row], next_before: null } }))
      await page.route('**/admin/logs?*', route => route.request().isNavigationRequest() ? route.fallback() : route.fulfill({ json: { data: [row] } }))
      await page.goto(path)
      await page.getByRole('button', { name: `展开 ${requestId} 的明细`, exact: true }).click()
      const drawer = page.getByRole('dialog', { name: '请求与账单详情' })
      const body = drawer.locator('[data-slot="log-detail-body"]')
      const family = await body.evaluate(node => getComputedStyle(node).fontFamily)
      const styles = await body.locator('[data-slot="detail-field"] > dd, [data-slot="detail-id"] > dd').evaluateAll(nodes => nodes.map(node => {
        const css = getComputedStyle(node)
        return { family: css.fontFamily, size: css.fontSize, weight: css.fontWeight, lineHeight: css.lineHeight }
      }))
      expect(styles.length).toBeGreaterThan(12)
      for (const css of styles) expect(css).toEqual({ family, size: '13px', weight: '400', lineHeight: '20px' })
      for (const label of await body.locator('[data-slot="detail-field"] > dt, [data-slot="detail-id"] > dt').all()) {
        expect(await label.evaluate(node => getComputedStyle(node).fontSize)).toBe('12px')
      }
      for (const title of await body.locator('[data-slot="detail-section"] h3').all()) {
        expect(await title.evaluate(node => [getComputedStyle(node).fontSize, getComputedStyle(node).fontWeight])).toEqual(['14px', '600'])
      }
      expect(await body.locator('[data-slot="detail-amount-value"]').evaluate(node => [getComputedStyle(node).fontSize, getComputedStyle(node).fontWeight])).toEqual(['24px', '600'])
      for (const stat of await body.locator('[data-slot="detail-stat"] > dd').all()) {
        expect(await stat.evaluate(node => [getComputedStyle(node).fontSize, getComputedStyle(node).fontWeight])).toEqual(['13px', '500'])
      }
      const request = body.locator('[data-slot="detail-id"]').filter({ has: page.getByText(requestId, { exact: true }) })
      await expect(request.locator('dd > span').first()).toHaveText(requestId)
      expect(await request.locator('dd > span').first().evaluate(node => getComputedStyle(node).fontFamily)).toBe(family)
      await drawer.screenshot({ path: `test-results/log-detail-type-${path.startsWith('/admin') ? 'admin' : 'portal'}-${theme}.png`, animations: 'disabled' })
      await request.getByRole('button', { name: '复制', exact: true }).focus()
      await page.keyboard.press('Enter')
      await expect.poll(() => page.evaluate(() => (window as Window & { logDetailCopied?: string }).logDetailCopied)).toBe(requestId)
      const billingTable = body.getByRole('table', { name: '快照计费分项' })
      await expect(billingTable).toContainText('缓存写入')
      expect(await billingTable.locator('tbody td').first().evaluate(node => getComputedStyle(node).fontSize)).toBe('13px')
      await page.keyboard.press('Escape')
      await expect(page.getByRole('tooltip')).toHaveCount(0)
      await page.keyboard.press('Escape')
      await expect(drawer).toHaveCount(0)
    })
  }
}

for (const path of ['/admin/logs', '/portal/logs?scope=user']) {
  test(`日志详情字体 ${path} 长字段：统一字号后完整名称、模型和ID不溢出`, async ({ page }) => {
    await prepare(page)
    await page.setViewportSize({ width: 390, height: 844 })
    const long = 'a-very-long-identifier-without-any-spaces-'.repeat(6)
    const row = { ...detailedLog, username: long, key_name: long, channel_name: long, requested_model: long,
      upstream_model: long, endpoint: `/v1/${long}`, request_id: 'req-20' }
    await page.route('**/api/me/logs?*', route => route.fulfill({ json: { data: [row], next_before: null } }))
    await page.route('**/admin/logs?*', route => route.request().isNavigationRequest() ? route.fallback() : route.fulfill({ json: { data: [row] } }))
    await page.goto(path)
    await page.getByRole('button', { name: '展开 req-20 的明细', exact: true }).click()
    const drawer = page.getByRole('dialog')
    const body = drawer.locator('[data-slot="log-detail-body"]')
    for (const node of await body.locator('[data-slot="detail-field"], [data-slot="detail-id"], [data-slot="detail-object"]').all()) {
      expect(await node.evaluate(node => node.scrollWidth <= node.clientWidth + 1)).toBe(true)
    }
    expect(await body.evaluate(node => node.scrollWidth <= node.clientWidth + 1)).toBe(true)
    await expect(body.getByText(long, { exact: true }).first()).toBeAttached()
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  })
}

for (const fixture of [
  { name: '未上报', usage: {}, visible: {} },
  { name: '空值', usage: { cache_write_5m_tokens: null, cache_write_1h_tokens: null, cache_read_modalities: null, cache_write_modalities: null }, visible: {} },
  { name: '时长含真实零', usage: { cache_write_5m_tokens: 100, cache_write_1h_tokens: 0 }, visible: { '其中 5 分钟写入': '100', '其中 1 小时写入': '0' } },
  { name: '读取含真实零', usage: { cache_read_modalities: { audio_tokens: 0, image_tokens: 40 } }, visible: { '缓存读取 · 音频': '0', '缓存读取 · 图片': '40' } },
  { name: '写入含真实零', usage: { cache_write_modalities: { audio_tokens: 30, image_tokens: 0 } }, visible: { '缓存写入 · 音频': '30', '缓存写入 · 图片': '0' } },
  { name: '时长拆分不完整', usage: { cache_write_5m_tokens: 100 }, visible: {} },
]) {
  for (const path of ['/portal/logs?scope=user', '/admin/logs']) {
    test(`缓存子项按实报展示 ${path} ${fixture.name}：保留总量与真实零，不生成缺失占位`, async ({ page }) => {
      await prepare(page)
      const row = { ...detailedLog, usage: { ...detailedLog.usage, ...fixture.usage } }
      await page.route('**/api/me/logs?*', (route) => route.fulfill({ json: { data: [row], next_before: null } }))
      await page.route('**/admin/logs?*', (route) => route.request().isNavigationRequest()
        ? route.fallback() : route.fulfill({ json: { data: [row] } }))
      await page.goto(path)
      await page.getByRole('button', { name: '展开 req-20 的明细' }).click()
      const breakdown = page.locator('section').filter({ has: page.getByRole('heading', { name: '用量拆分', exact: true }) })
      await expect(breakdown).toHaveCount(1)
      for (const label of ['缓存读取', '缓存写入']) {
        await expect(breakdown.locator('dt').getByText(label, { exact: true }).locator('..').locator('dd')).toHaveText('100')
      }
      const subsets = breakdown.locator('[data-slot="cache-subsets"]')
      const visible: Record<string, string> = fixture.visible
      await expect(subsets).toHaveCount(Object.keys(visible).length ? 1 : 0)
      for (const label of cacheSubsetLabels) {
        const term = breakdown.locator('dt').getByText(label, { exact: true })
        if (visible[label] === undefined) await expect(term).toHaveCount(0)
        else await expect(term.locator('..').locator('dd')).toHaveText(visible[label])
      }
      if (Object.keys(visible).length) await expect(subsets).not.toContainText('未上报')
      if (path.startsWith('/portal/') && fixture.name === '时长含真实零') {
        await subsets.scrollIntoViewIfNeeded()
        await page.screenshot({ path: 'test-results/cache-subsets-reported-only.png', animations: 'disabled' })
      }
    })
  }
}

for (const fixture of [
  { name: '缺失', values: {}, shown: false },
  { name: '实报零', values: { cache_write_5m_tokens: 0, cache_write_1h_tokens: 0, cache_write_ttl_samples: 1,
    cache_read_audio_tokens: 0, cache_read_image_tokens: 0, cache_read_modal_samples: 1,
    cache_write_audio_tokens: 0, cache_write_image_tokens: 0, cache_write_modal_samples: 1 }, shown: true },
  { name: '无实报样本的占位零', values: { cache_write_5m_tokens: 0, cache_write_1h_tokens: 0, cache_write_ttl_samples: 0,
    cache_read_audio_tokens: 0, cache_read_image_tokens: 0, cache_read_modal_samples: 0,
    cache_write_audio_tokens: 0, cache_write_image_tokens: 0, cache_write_modal_samples: 0 }, shown: false },
]) {
  test(`缓存子项按实报展示 汇总 ${fixture.name}：不把缺失样本当成零`, async ({ page }) => {
    await prepare(page)
    await page.route('**/api/me/logs/stat?*', (route) => route.fulfill({ json: {
      records: 1, settled: 1, failed: 0, refunded: 0, amount_micro: 4696, prompt_tokens: 1000,
      completion_tokens: 500, cached_tokens: 100, cache_read_samples: 1, cache_write_tokens: 100, cache_write_samples: 1,
      ...fixture.values,
    } }))
    await page.goto('/portal/logs?scope=user')
    const summary = page.getByRole('region', { name: '筛选范围汇总' })
    await summary.getByText('更多用量指标', { exact: true }).click()
    await expect(summary.getByText('缓存写入', { exact: true }).locator('..')).toContainText('100')
    for (const label of cacheSubsetLabels) {
      const term = summary.locator('dt').getByText(label, { exact: true })
      if (fixture.shown) await expect(term.locator('..').locator('dd').first()).toHaveText('0')
      else await expect(term).toHaveCount(0)
    }
  })
}

test('多模态快照：缓存交集和图片输出只收费一次，TTL不增加总量，缺单价不猜测', () => {
  const row = { ...detailedLog, usage: { ...detailedLog.usage,
    cached_tokens: 200, cache_write_tokens: 100, cache_write_5m_tokens: 60, cache_write_1h_tokens: 40,
    cache_read_modalities: { audio_tokens: 30, image_tokens: 20 }, cache_write_modalities: { audio_tokens: 10, image_tokens: 15 },
    audio_prompt_tokens: 40, image_prompt_tokens: 50, audio_completion_tokens: 80, image_completion_tokens: 120,
  }, pricing_snapshot: { ...detailedLog.pricing_snapshot, audio_ratio: '16', audio_completion_ratio: '2', image_ratio: '2',
    modality_ratios: { audio_cache_read: '8', image_cache_read: '0.5', audio_cache_write: '20', image_cache_write: '2.5', image_output: '3' }, service_tier: 'priority', tier_ratio: '2' } }
  const lines = billingLines(row)
  expect(lines.reduce((total, line) => total + (line.quantity ?? 0), 0)).toBe(1500)
  expect(lines.find((line) => line.name === 'normalInput')?.quantity).toBe(610)
  expect(lines.find((line) => line.name === 'textOutput')?.quantity).toBe(300)
  expect(lines.find((line) => line.name === 'cacheReadText')?.quantity).toBe(150)
  expect(lines.find((line) => line.name === 'cacheWriteText')?.quantity).toBe(75)
  expect(lines.find((line) => line.name === 'imageOutput')).toMatchObject({ quantity: 120, unitMicro: 4_800_000, amountMicro: 576 })
  expect(lines.find((line) => line.name === 'cacheReadAudio')).toMatchObject({ quantity: 30, unitMicro: 12_800_000, amountMicro: 384 })
  expect(billingLines({ ...row, pricing_snapshot: { ...row.pricing_snapshot, modality_ratios: {} } }).find((line) => line.name === 'imageOutput')?.amountMicro).toBeNull()
  expect(billingLines({ ...row, usage: { ...row.usage, image_completion_tokens: null } })).toEqual([])
})

test('新增用量指标：汇总保留覆盖率，详情显示TTL、图片输出、来源和服务层级', async ({ page }) => {
  await prepare(page)
  await page.setViewportSize({ width: 1440, height: 1000 })
  const row = { ...detailedLog, usage: { ...detailedLog.usage, image_completion_tokens: 20,
    cache_write_5m_tokens: 60, cache_write_1h_tokens: 40, prompt_source: 'local_override', completion_source: 'upstream', upstream_usage: { prompt_tokens: 1200, completion_tokens: 500 },
  }, pricing_snapshot: { ...detailedLog.pricing_snapshot, service_tier: 'priority', tier_ratio: '2', modality_ratios: { image_output: '3' } } }
  await page.route('**/api/me/logs?*', (route) => route.fulfill({ json: { data: [row], next_before: null } }))
  await page.route('**/api/me/logs/stat?*', (route) => route.fulfill({ json: { records: 10, settled: 10, failed: 0, refunded: 0, amount_micro: 4696, prompt_tokens: 1000, completion_tokens: 500, cache_read_samples: 1, cached_tokens: 100,
    cache_write_tokens: 100, cache_write_samples: 1, cache_write_5m_tokens: 60, cache_write_1h_tokens: 40, cache_write_ttl_samples: 1, image_completion_tokens: 20, image_completion_samples: 1,
  } }))
  await page.goto('/portal/logs?scope=user')
  const summary = page.getByRole('region', { name: '筛选范围汇总' })
  await summary.getByText('更多用量指标', { exact: true }).click()
  await expect(summary).toContainText('已上报 1 / 10 条；缺失不作零')
  await expect(summary.getByText('其中 5 分钟写入', { exact: true }).locator('..')).toContainText('60')
  await expect(summary.getByText('图片输出', { exact: true }).locator('..')).toContainText('20')
  await summary.getByText('更多用量指标', { exact: true }).click()
  await page.getByRole('button', { name: '展开 req-20 的明细' }).click()
  const detail = page.getByRole('dialog', { name: '请求与账单详情' })
  await expect(detail).toContainText('本地覆盖')
  await expect(detail).toContainText('上游实报')
  await expect(detail).toContainText('1,200')
  await expect(detail).toContainText('其中 5 分钟写入')
  await expect(detail).toContainText('服务层级 priority ×2')
  await expect(detail.getByText('平均输出速度', { exact: true }).locator('..')).toContainText('625 tok/s')
  await expect(detail.getByText('生成速度（估算）', { exact: true }).locator('..')).toContainText('714.3 tok/s')
  await detail.getByText('缓存子项', { exact: true }).scrollIntoViewIfNeeded()
  await page.screenshot({ path: 'test-results/extended-token-breakdown.png', animations: 'disabled' })
})

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
    const reads = cells.locator('[data-slot="cache-read"]'), writes = cells.locator('[data-slot="cache-write"]')
    await expect(cells).toHaveCount(8)
    await expect(table.getByRole('columnheader')).toHaveCount(8)
    await expect(table.getByRole('columnheader', { name: '缓存读 / 写', exact: true })).toHaveCount(0)
    await expect(cells.nth(0)).toContainText('输入1,000')
    await expect(cells.nth(0)).toContainText('输出500')
    await expect(reads.nth(0)).toHaveText('80080%')
    await expect(reads.nth(0)).toHaveAttribute('data-state', 'hit')
    await expect(cells.nth(0).getByLabel('缓存读取占输入 80%')).toBeVisible()
    await expect(writes.nth(0)).toHaveText('100')
    await expect(writes.nth(0)).toHaveAttribute('data-state', 'write')
    await expect(reads.nth(1)).toHaveText('0')
    await expect(writes.nth(1)).toHaveText('1,000')
    await expect(reads.nth(2)).toHaveText('0')
    await expect(writes.nth(2)).toHaveText('0')
    for (const index of [1, 2]) {
      await expect(reads.nth(index)).toHaveAttribute('data-state', 'empty')
      await expect(reads.nth(index).locator('.lucide-zap-off')).toHaveCount(1)
    }
    for (const index of [3, 4, 5]) {
      await expect(reads.nth(index)).toHaveText('—')
      await expect(reads.nth(index)).toHaveAttribute('data-state', 'missing')
      await expect(reads.nth(index)).toHaveClass(/border-dashed/)
    }
    for (const index of [3, 4]) {
      await expect(writes.nth(index)).toHaveText('—')
      await expect(writes.nth(index)).toHaveAttribute('data-state', 'missing')
      await expect(writes.nth(index)).toHaveClass(/border-dashed/)
    }
    await expect(reads.nth(3)).toHaveAccessibleName('缓存读取 未上报')
    await expect(reads.nth(4)).toHaveAccessibleName('缓存读取 未记录')
    await expect(writes.nth(5)).toHaveText('100')
    await expect(cells.locator('[data-slot="token-cache"]').filter({ hasText: /未上报|未记录|未命中|写入/ })).toHaveCount(0)
    await expect(cells.nth(6)).toContainText('输入12,345,678')
    await expect(writes.nth(6)).toHaveText('1,000,000')
    await expect(cells.nth(7)).toContainText('<0.1%')
    for (const row of await table.locator('tbody tr').all()) expect(Math.abs(await row.evaluate((node) => node.offsetHeight) - 44)).toBeLessThanOrEqual(1)
    for (const cell of await cells.all()) expect((await cell.boundingBox())!.x).toBeCloseTo((await cells.first().boundingBox())!.x, 1)
    for (const tag of await cells.first().locator('[data-slot="cache-read"], [data-slot="cache-write"]').all()) {
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

for (const language of ['zh-CN', 'en']) {
  test(`缓存图标提示 ${language}：悬停与键盘可读，实报零和缺失可辨，保留明细入口`, async ({ page }) => {
    await prepare(page)
    await page.addInitScript((language) => localStorage.setItem('okapi.lang', language), language)
    const records = [
      { ...detailedLog, usage: { ...detailedLog.usage, cached_tokens: 800 } },
      { ...detailedLog, usage: { ...detailedLog.usage, cached_tokens: 0, cache_write_tokens: 0 } },
      { ...detailedLog, usage: { ...detailedLog.usage, cached_tokens: 0, cache_read_reported: false, cache_write_reported: false, cache_write_tokens: null } },
      { ...log, usage: { ...log.usage, cached_tokens: 0 } },
    ].map((row, i) => ({ ...row, id: 20 - i, request_id: `req-${20 - i}` }))
    await page.route('**/api/me/logs?*', (route) => route.fulfill({ json: { data: records, next_before: null } }))
    await page.goto('/portal/logs?scope=user')
    const cells = page.locator('[data-slot="log-token-usage"]')
    const reads = cells.locator('[data-slot="cache-read"]'), writes = cells.locator('[data-slot="cache-write"]')
    const tip = page.getByRole('tooltip')
    await reads.nth(0).hover()
    await expect(tip).toContainText('800')
    await expect(tip).toContainText('80%')
    await expect(reads.nth(0)).toHaveAccessibleDescription(language === 'en' ? /billing snapshot/ : /账单快照/)
    await page.mouse.move(0, 0)
    await page.keyboard.press('Escape')
    await expect(tip).toHaveCount(0)
    await reads.nth(1).focus()
    await expect(tip).toContainText(language === 'en' ? 'reported zero' : '已上报：本次缓存读取为 0')
    await page.keyboard.press('Escape')
    await writes.nth(1).focus()
    await expect(tip).toContainText(language === 'en' ? '0 tokens written' : '缓存写入 0')
    await page.keyboard.press('Escape')
    for (const [index, state] of [[2, language === 'en' ? 'Not reported' : '未上报'], [3, language === 'en' ? 'Not recorded' : '未记录']] as const) {
      await reads.nth(index).focus()
      await expect(tip).toContainText(state)
      await expect(reads.nth(index)).toHaveAccessibleDescription(language === 'en' ? /unknown/ : /不能判断是否命中/)
      await page.keyboard.press('Escape')
      await writes.nth(index).focus()
      await expect(tip).toContainText(state)
      await expect(writes.nth(index)).toHaveAccessibleDescription(language === 'en' ? /not treated as zero/ : /不按 0 展示/)
      await page.keyboard.press('Escape')
    }
    const caches = cells.locator('[data-slot="token-cache"]')
    for (const cache of await caches.all()) expect((await cache.boundingBox())!.width).toBeLessThanOrEqual(384)
    await page.getByRole('button', { name: language === 'en' ? 'Expand details for req-20' : '展开 req-20 的明细' }).click()
    await expect(page.getByRole('dialog')).toBeVisible()
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
  await expect(page.getByRole('navigation', { name: '分页' })).toContainText('1–3 / 共 77 条')
  await expect(page.getByRole('table').getByText('已退款')).toBeVisible()
  await expect(page.getByRole('table').getByText('非流式')).toBeVisible()
  await expect(page.getByRole('button', { name: '导出本页 CSV' })).toHaveAttribute('title', '仅导出当前页的 3 条，不是全部筛选结果。')
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
  const audioInput = detail.locator('dt').filter({ hasText: /^音频输入（非缓存）$/ }).locator('..').locator('dd')
  await expect(audioInput).toHaveText('—')
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
      await expect(table.locator('[data-slot="log-token-usage"]')).toHaveCount(1)
      await expect(table.getByRole('columnheader', { name: '响应速度', exact: true })).toBeVisible()
      await table.locator('tbody tr').click()
      const dialog = page.getByRole('dialog')
      await expect(dialog).toBeVisible()
      await expect(table.locator('tbody tr')).toHaveCount(1)
      await page.keyboard.press('Escape')
      await expect(dialog).toHaveCount(0)
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

async function pagedLogs(page: Page) {
  await prepare(page)
  const queries: URL[] = []
  const rows = Array.from({ length: 43 }, (_, index) => ({ ...detailedLog, id: 100 - index, request_id: `paged-${100 - index}` }))
  await page.route('**/api/me/logs/stat?*', (route) => {
    queries.push(new URL(route.request().url()))
    return route.fulfill({ json: { records: 43, settled: 43, failed: 0, refunded: 0, pending: 0, amount_micro: 123456, refunded_amount_micro: 0, prompt_tokens: 43000, completion_tokens: 21500, cached_tokens: 4300, cache_read_samples: 43, avg_latency_ms: 800, latency_samples: 43, avg_ttft_ms: 100, ttft_samples: 43 } })
  })
  await page.route('**/api/me/logs?*', (route) => {
    const url = new URL(route.request().url())
    queries.push(url)
    const limit = Number(url.searchParams.get('limit'))
    const before = Number(url.searchParams.get('before') ?? 101)
    const remaining = rows.filter((row) => row.id < before)
    const data = remaining.slice(0, limit)
    return route.fulfill({ json: { data, next_before: remaining.length > limit ? data.at(-1)!.id : null } })
  })
  return queries
}

test('门户分页：逐页替换、末页和缓存返回、每页条数切换，统计不跟随翻页，CSV 只导出本页', async ({ page }) => {
  const queries = await pagedLogs(page)
  await page.goto('/portal/logs?scope=user&model=gpt-alpha&api_key_id=1')
  const rows = page.getByRole('table').locator('tbody tr')
  const footer = page.getByRole('navigation', { name: '分页' })
  const summary = page.getByRole('region', { name: '筛选范围汇总' })
  const listQueries = () => queries.filter((url) => url.pathname === '/api/me/logs')
  const statQueries = () => queries.filter((url) => url.pathname.endsWith('/stat'))
  await expect(rows).toHaveCount(10)
  await expect(footer).toContainText('1–10 / 共 43 条')
  expect(listQueries()[0].searchParams.get('limit')).toBe('10')
  await expect(footer.getByRole('button', { name: '上一页' })).toBeDisabled()
  const statsText = await summary.innerText(), statsCount = statQueries().length
  await page.locator('[data-slot="table-viewport"]').evaluate((el) => { el.scrollTop = 300 })
  await footer.getByRole('button', { name: '下一页' }).click()
  await expect(footer).toContainText('11–20 / 共 43 条')
  await expect(rows).toHaveCount(10)
  await expect(page.getByRole('button', { name: '展开 paged-100 的明细', exact: true })).toHaveCount(0)
  expect(await page.locator('[data-slot="table-viewport"]').evaluate((el) => el.scrollTop)).toBe(0)
  for (const number of [3, 4, 5]) {
    await footer.getByRole('button', { name: '下一页' }).click()
    await expect(footer).toContainText(`第 ${number} 页`)
  }
  await expect(footer).toContainText('41–43 / 共 43 条')
  await expect(rows).toHaveCount(3)
  await expect(footer.getByRole('button', { name: '下一页' })).toBeDisabled()
  const [download] = await Promise.all([page.waitForEvent('download'), page.getByRole('button', { name: '导出本页 CSV' }).click()])
  const csv = await readFile((await download.path())!, 'utf8')
  expect(csv.trim().split('\n')).toHaveLength(4)
  expect(csv).toContain('paged-60')
  expect(csv).not.toContain('paged-100')
  const count = listQueries().length
  await footer.getByRole('button', { name: '上一页' }).click()
  await expect(footer).toContainText('第 4 页')
  await expect(rows).toHaveCount(10)
  expect(listQueries()).toHaveLength(count)
  expect(await summary.innerText()).toBe(statsText)
  expect(statQueries()).toHaveLength(statsCount)
  for (const size of ['50', '100', '20', '10']) {
    await footer.getByRole('combobox', { name: '每页条数' }).selectOption(size)
    await expect(rows).toHaveCount(Math.min(Number(size), 43))
    await expect(footer).toContainText('第 1 页')
    await expect(footer.getByRole('button', { name: '上一页' })).toBeDisabled()
  }
  expect(statQueries()).toHaveLength(statsCount)
  for (const url of listQueries()) {
    expect(url.searchParams.get('model')).toBe('gpt-alpha')
    expect(url.searchParams.get('api_key_id')).toBe('1')
    expect(url.searchParams.get('scope')).toBe('user')
  }
  for (const url of statQueries()) {
    expect(url.searchParams.has('before')).toBe(false)
    expect(url.searchParams.has('limit')).toBe(false)
  }
})

test('门户分页失败保留当前页且可重试；刷新重置游标，筛选及页宽不带旧页数据', async ({ page }) => {
  const queries = await pagedLogs(page)
  let fail = true
  await page.route('**/api/me/logs?*', async (route) => {
    if (new URL(route.request().url()).searchParams.has('before') && fail) {
      return route.fulfill({ status: 503, json: { error: { code: 'internal_error' } } })
    }
    return route.fallback()
  })
  await page.goto('/portal/logs?scope=user')
  const footer = page.getByRole('navigation', { name: '分页' })
  const next = footer.getByRole('button', { name: '下一页' })
  await expect(page.getByRole('table').locator('tbody tr')).toHaveCount(10)
  await next.click()
  await expect(page.getByRole('alert')).toBeVisible()
  await expect(footer).toContainText('第 1 页')
  await expect(page.getByRole('table').locator('tbody tr')).toHaveCount(10)
  await expect(next).toBeEnabled()
  fail = false
  await next.click()
  await expect(footer).toContainText('第 2 页')
  await expect(page.getByRole('alert')).toHaveCount(0)
  const count = queries.length
  await page.getByRole('button', { name: '刷新', exact: true }).click()
  await expect(footer).toContainText('第 1 页')
  await expect(page.getByRole('button', { name: '展开 paged-100 的明细', exact: true })).toBeVisible()
  await expect.poll(() => queries.slice(count).filter((url) => url.pathname === '/api/me/logs').length).toBe(1)
  expect(queries.slice(count).filter((url) => url.pathname === '/api/me/logs').every((url) => !url.searchParams.has('before'))).toBe(true)
  await next.click()
  await expect(footer).toContainText('第 2 页')
  await page.getByRole('switch', { name: '只看失败' }).click()
  await expect(footer).toContainText('第 1 页')
  await expect.poll(() => queries.filter((url) => url.pathname === '/api/me/logs').at(-1)?.searchParams.get('errors_only')).toBe('true')
  expect(queries.filter((url) => url.pathname === '/api/me/logs').at(-1)?.searchParams.has('before')).toBe(false)
})

test('门户分页边界：加载时锁定分页与导出，旧接口的多余末页游标不会跳进空页', async ({ page }) => {
  await pagedLogs(page)
  let release: () => void = () => {}
  const gate = new Promise<void>((resolve) => { release = resolve })
  let nextCalls = 0
  await page.route('**/api/me/logs?*', async (route) => {
    if (!new URL(route.request().url()).searchParams.has('before')) return route.fallback()
    nextCalls += 1
    await gate
    return route.fulfill({ json: { data: [], next_before: null } })
  })
  await page.goto('/portal/logs?scope=user')
  const footer = page.getByRole('navigation', { name: '分页' })
  const next = footer.getByRole('button', { name: '下一页' })
  const summary = page.getByRole('region', { name: '筛选范围汇总' })
  await summary.getByRole('button', { name: '缓存读取' }).focus()
  await expect(page.getByRole('tooltip')).toContainText('已上报 43 / 43 条')
  await page.keyboard.press('Escape')
  await next.click()
  await expect(next).toBeDisabled()
  await expect(footer.getByRole('combobox')).toBeDisabled()
  await expect(page.getByRole('button', { name: '导出本页 CSV' })).toBeDisabled()
  expect(nextCalls).toBe(1)
  release()
  await expect(footer.getByRole('combobox')).toBeEnabled()
  await expect(next).toBeDisabled()
  await expect(footer).toContainText('第 1 页')
  await expect(page.getByRole('table').locator('tbody tr')).toHaveCount(10)
  await expect(page.getByRole('button', { name: '导出本页 CSV' })).toBeEnabled()
})

for (const { width, height, language } of [
  { width: 1366, height: 768, language: 'zh-CN' }, { width: 1440, height: 900, language: 'zh-CN' },
  { width: 1440, height: 900, language: 'en' }, { width: 1920, height: 1080, language: 'zh-CN' },
]) {
  test(`管理日志数据密度 ${width}x${height} ${language}：压缩工具区，长名称不撑宽，分页常驻`, async ({ page }) => {
    await prepare(page)
    await page.addInitScript((language) => {
      localStorage.setItem('okapi.lang', language)
      localStorage.setItem('okapi.theme', language === 'en' ? 'dark' : 'light')
    }, language)
    await page.setViewportSize({ width, height })
    await page.emulateMedia({ reducedMotion: 'reduce' })
    const rows = Array.from({ length: 50 }, (_, index) => ({
      ...detailedLog, request_id: `density-${index}`,
      username: `user-with-a-long-name-${index}`, channel_name: `channel-with-a-long-name-${index}`,
      model: `modal-3c25fa6193c34e5ab24f49b5c95409dd-${index}`, client_type: 'long-client-identifier',
    }))
    await page.route('**/admin/logs?*', (route) => route.request().isNavigationRequest()
      ? route.fallback()
      : route.fulfill({ json: { data: rows } }))
    await page.goto('/admin/logs?limit=50')
    const table = page.getByRole('table'), frame = page.locator('[data-slot="table-frame"]')
    await expect(table.locator('tbody tr')).toHaveCount(50)
    const box = (await frame.boundingBox())!
    expect(box.y).toBeLessThanOrEqual(300)
    expect(box.height).toBeGreaterThanOrEqual(height - 390)
    expect((await page.locator('[data-slot="admin-log-filters"]').boundingBox())!.height).toBeLessThanOrEqual(110)
    expect((await page.locator('[data-slot="admin-log-summary"]').boundingBox())!.height).toBeLessThanOrEqual(70)
    await expect(page.locator('[data-slot="pagination"]')).toBeInViewport({ ratio: 1 })
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    const viewport = page.locator('[data-slot="table-viewport"]')
    const horizontal = await viewport.evaluate((node) => node.scrollWidth > node.clientWidth + 1)
    await expect(frame.getByRole('group')).toHaveCount(horizontal ? 1 : 0)
    const columnControlsHeight = horizontal ? (await frame.getByRole('group').boundingBox())!.height : 0
    const scrollbarHeight = await viewport.evaluate((node) => node.offsetHeight - node.clientHeight)
    const visible = await table.locator('tbody tr').evaluateAll((nodes) => {
      const viewport = nodes[0].closest('[data-slot="table-viewport"]')!.getBoundingClientRect()
      const header = nodes[0].closest('table')!.querySelector('thead')!.getBoundingClientRect()
      return nodes.filter((node) => {
        const row = node.getBoundingClientRect()
        return row.top >= header.bottom - 1 && row.bottom <= viewport.bottom + 1
      }).length
    })
    expect(visible).toBeGreaterThanOrEqual(Math.floor((height - 380) / 44) - 1 - Math.ceil((columnControlsHeight + scrollbarHeight) / 44))
    expect(Math.abs((await table.locator('tbody tr').first().boundingBox())!.height - 44)).toBeLessThanOrEqual(1)
    await expect(table.locator('tbody tr').first().locator('td').nth(4)).toHaveAttribute('title', rows[0].model)
    const headerY = (await table.locator('thead').boundingBox())!.y
    await viewport.evaluate((node) => { node.scrollTop = 250 })
    expect(Math.abs((await table.locator('thead').boundingBox())!.y - headerY)).toBeLessThan(1)
    await viewport.evaluate((node) => { node.scrollTop = 0 })
    await page.screenshot({ path: `test-results/admin-log-density-${width}-${language}.png`, animations: 'disabled' })
  })
}

for (const width of [390, 1366, 1920]) test(`管理日志失败信息 ${width}px：状态和用户可读，错误记录与完整详情可见`, async ({ page }) => {
  await prepare(page)
  await page.setViewportSize({ width, height: 900 })
  await page.emulateMedia({ reducedMotion: 'reduce' })
  await page.context().grantPermissions(['clipboard-read', 'clipboard-write'])
  const longCode = `provider_error_${'specific-detail-'.repeat(18)}`
  const records = [
    { ...log, request_id: 'failed-batch', username: 'hold-fixture-01', model: 'batch-image', log_type: 5, status: 40,
      error_code: 'batch_failed', is_error: true, upstream_status: 200, amount_micro: 0 },
    { ...log, request_id: 'failed-provider', error_code: longCode, log_type: 5, status: 40, is_error: true, upstream_status: 502 },
    { ...log, request_id: 'failed-no-code', error_code: '', log_type: 5, status: 40, is_error: true, upstream_status: 0 },
    { ...log, request_id: 'successful' },
  ]
  const queries: URL[] = []
  await page.route('**/admin/logs?*', (route) => {
    if (route.request().isNavigationRequest()) return route.fallback()
    const url = new URL(route.request().url())
    queries.push(url)
    return route.fulfill({ json: { data: url.searchParams.get('errors_only') === 'true' ? records.filter((row) => row.is_error) : records } })
  })
  await page.goto('/admin/logs')
  const table = page.getByRole('table', { name: '日志', exact: true }), rows = table.locator('tbody tr')
  await expect(rows).toHaveCount(4)
  await expect(page.getByRole('switch', { name: '只看失败' })).not.toBeChecked()
  expect(queries[0].searchParams.has('errors_only')).toBe(false)
  const cells = rows.first().locator('td')
  await expect(cells.nth(2).getByText('失败', { exact: true })).toBeVisible()
  await expect(cells.nth(2).getByText('batch_failed', { exact: true })).toBeVisible()
  await expect(cells.nth(3)).toHaveText('hold-fixture-01')
  expect((await cells.nth(2).boundingBox())!.width).toBeGreaterThanOrEqual(104)
  expect((await cells.nth(3).boundingBox())!.width).toBeGreaterThanOrEqual(120)
  for (const locator of [cells.nth(2).getByText('batch_failed', { exact: true }), cells.nth(3)]) {
    expect(await locator.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true)
  }
  expect((await cells.nth(4).boundingBox())!.width).toBeGreaterThanOrEqual(112)
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  expect(Math.abs((await rows.first().boundingBox())!.height - (width < 768 ? 56 : 44))).toBeLessThanOrEqual(1)
  await page.screenshot({ path: `test-results/admin-log-failures-${width}.png`, animations: 'disabled' })

  await page.getByRole('button', { name: '展开 failed-batch 的明细', exact: true }).click()
  const drawer = page.getByRole('dialog'), error = drawer.getByRole('region', { name: '错误信息', exact: true })
  await expect(error).toContainText('批量任务未成功生成结果。')
  await expect(error.locator('dt').getByText('错误码', { exact: true }).locator('..').locator('dd')).toHaveText('batch_failed')
  await expect(error.locator('dt').getByText('上游状态码', { exact: true }).locator('..').locator('dd')).toHaveText('200')
  await expect(error).toBeInViewport({ ratio: 1 })
  await drawer.getByRole('button', { name: '关闭', exact: true }).click()
  await page.getByRole('button', { name: '展开 failed-provider 的明细', exact: true }).click()
  await expect(error.locator('dd').first()).toHaveText(longCode)
  expect(await error.evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true)
  await error.getByRole('button', { name: '复制', exact: true }).click()
  await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toBe(longCode)
  await drawer.getByRole('button', { name: '关闭', exact: true }).click()
  await page.getByRole('button', { name: '展开 failed-no-code 的明细', exact: true }).click()
  await expect(error).toContainText('这条失败记录未记录错误码。')
  await expect(error.locator('dd')).toHaveText('—')
  await drawer.getByRole('button', { name: '关闭', exact: true }).click()
  await page.getByRole('button', { name: '展开 successful 的明细', exact: true }).click()
  await expect(error).toHaveCount(0)
  await drawer.getByRole('button', { name: '关闭', exact: true }).click()

  await page.getByRole('switch', { name: '只看失败' }).click()
  await page.getByRole('button', { name: '搜索', exact: true }).click()
  await expect(rows).toHaveCount(3)
  expect(queries.at(-1)?.searchParams.get('errors_only')).toBe('true')
  await expect(rows.getByText('成功', { exact: true })).toHaveCount(0)
})

for (const language of ['zh-CN', 'en']) test(`门户错误信息 ${language}：完整错误码置顶，缺失错误码不隐藏失败详情`, async ({ page }) => {
  await prepare(page)
  await page.addInitScript((lang) => localStorage.setItem('okapi.lang', lang), language)
  await page.setViewportSize({ width: 390, height: 800 })
  await page.route('**/api/me/logs?*', (route) => route.fulfill({ json: { data: [
    { ...log, status: 40, error_code: 'upstream_error' },
    { ...log, id: 19, request_id: 'req-19', status: 40, error_code: null },
    { ...log, id: 18, request_id: 'req-18', status: 30, error_code: 'batch_failed' },
  ], next_before: null } }))
  await page.goto('/portal/logs')
  await page.getByRole('button', { name: language === 'en' ? 'Expand details for req-20' : '展开 req-20 的明细' }).click()
  const drawer = page.getByRole('dialog'), error = drawer.getByRole('region', { name: language === 'en' ? 'Error information' : '错误信息', exact: true })
  await expect(error).toBeInViewport({ ratio: 1 })
  await expect(error).toContainText(language === 'en' ? 'Upstream error' : '上游服务错误')
  await expect(error.locator('dd')).toHaveText('upstream_error')
  await drawer.getByRole('button', { name: language === 'en' ? 'Close' : '关闭', exact: true }).click()
  await page.getByRole('button', { name: language === 'en' ? 'Expand details for req-19' : '展开 req-19 的明细' }).click()
  await expect(error).toContainText(language === 'en' ? 'No error code was recorded' : '未记录错误码')
  await expect(error.locator('dd')).toHaveText('—')
  await drawer.getByRole('button', { name: language === 'en' ? 'Close' : '关闭', exact: true }).click()
  await page.getByRole('button', { name: language === 'en' ? 'Expand details for req-18' : '展开 req-18 的明细' }).click()
  await expect(error).toContainText(language === 'en' ? 'did not produce successful results' : '批量任务未成功生成结果')
  await expect(drawer.getByText(language === 'en' ? 'Refunded' : '已退款', { exact: true })).toBeVisible()
})

test('管理日志紧凑工具栏：高级筛选和完整指标仍可展开，翻页后从首行开始', async ({ page }) => {
  await prepare(page)
  await page.setViewportSize({ width: 1440, height: 900 })
  await page.emulateMedia({ reducedMotion: 'reduce' })
  await page.route('**/admin/logs?*', (route) => {
    const params = new URL(route.request().url()).searchParams
    const offset = Number(params.get('offset') ?? 0), limit = Number(params.get('limit'))
    return route.fulfill({ json: { data: Array.from({ length: limit }, (_, index) => ({ ...detailedLog, request_id: `density-${offset + index}` })) } })
  })
  await page.goto('/admin/logs')
  await expect(page.getByRole('table').locator('tbody tr')).toHaveCount(10)
  await expect(page.getByRole('combobox', { name: '每页条数' })).toHaveValue('10')
  const filters = page.getByRole('button', { name: '更多筛选', exact: true })
  await filters.click()
  await expect(filters).toHaveAttribute('aria-expanded', 'true')
  await expect(page.locator('#lf-user_id')).toBeVisible()
  await filters.click()
  const summary = page.locator('[data-slot="admin-log-summary"]')
  const metrics = summary.getByRole('button', { name: '更多用量指标', exact: true })
  await metrics.click()
  await expect(summary.getByText('图片输出', { exact: true })).toBeVisible()
  await metrics.click()
  const viewport = page.locator('[data-slot="table-viewport"]')
  await viewport.evaluate((node) => { node.scrollTop = 250 })
  await page.getByRole('button', { name: '下一页', exact: true }).click()
  await expect(page.getByRole('button', { name: '展开 density-10 的明细', exact: true })).toBeVisible()
  expect(await viewport.evaluate((node) => node.scrollTop)).toBe(0)
})

for (const { width, height, language } of [
  { width: 1366, height: 768, language: 'zh-CN' }, { width: 1440, height: 900, language: 'zh-CN' },
  { width: 1440, height: 900, language: 'en' }, { width: 1920, height: 1080, language: 'zh-CN' },
]) {
  test(`门户日志紧凑分页 ${width}x${height} ${language}：表格获得剩余高度，底部分页常驻`, async ({ page }) => {
    await pagedLogs(page)
    await page.addInitScript((language) => {
      localStorage.setItem('okapi.lang', language)
      localStorage.setItem('okapi.theme', language === 'en' ? 'dark' : 'light')
    }, language)
    await page.setViewportSize({ width, height })
    await page.emulateMedia({ reducedMotion: 'reduce' })
    await page.goto('/portal/logs?scope=user')
    await expect(page.getByRole('table').locator('tbody tr')).toHaveCount(10)
    const frame = page.locator('[data-slot="table-frame"]')
    const footer = page.locator('[data-slot="pagination"]')
    const box = (await frame.boundingBox())!, bottom = (await footer.boundingBox())!
    expect(box.y).toBeLessThanOrEqual(370)
    expect(box.height).toBeGreaterThanOrEqual(height - 460)
    expect(bottom.y).toBeGreaterThanOrEqual(box.y + box.height)
    await expect(footer).toBeInViewport({ ratio: 1 })
    expect((await page.locator('[data-slot="log-filters"]').boundingBox())!.height).toBeLessThanOrEqual(110)
    const summary = page.getByRole('region', { name: language === 'en' ? 'Filtered summary' : '筛选范围汇总' })
    expect((await summary.boundingBox())!.height).toBeLessThanOrEqual(92)
    await expect(summary.locator('dl > div')).toHaveCount(6)
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    const viewport = page.locator('[data-slot="table-viewport"]')
    const headerY = (await page.getByRole('table').locator('thead').boundingBox())!.y
    await viewport.evaluate((el) => { el.scrollTop = 250 })
    expect(Math.abs((await page.getByRole('table').locator('thead').boundingBox())!.y - headerY)).toBeLessThan(1)
    await viewport.evaluate((el) => { el.scrollTop = 0 })
    await page.screenshot({ path: `test-results/logs-paged-${width}-${language}.png`, animations: 'disabled' })
  })
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
  await expect(page.getByRole('listbox').getByRole('option')).toHaveCount(1)
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
  await expect(period).toContainText('近 30 天')
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
    return route.fulfill({ json: { scope: 'user', data: first ? Array.from({ length: 10 }, (_, i) => ({ ...log, id: 100 - i, request_id: `req-${100 - i}` })) : [{ ...log, id: 90, request_id: 'req-90' }], next_before: first ? 91 : null } })
  })
  await page.goto('/portal/logs?scope=user&model=gpt-alpha&errors_only=true&start_date=2024-11-03&end_date=2024-11-03&timezone=America%2FLos_Angeles')
  await page.getByRole('button', { name: '下一页', exact: true }).click()
  await expect(page.getByRole('button', { name: '展开 req-90 的明细', exact: true })).toBeAttached()
  expect(Object.fromEntries(queries.filter((url) => url.pathname === '/api/me/logs').at(-1)!.searchParams)).toMatchObject({ before: '91', limit: '10', model: 'gpt-alpha', errors_only: 'true', scope: 'user', start_date: '2024-11-03', end_date: '2024-11-03', timezone: 'America/Los_Angeles' })
  await expect(page.getByRole('table').locator('tbody tr')).toHaveCount(1)
  await page.getByRole('button', { name: '上一页', exact: true }).click()
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
  await expect(page.getByRole('button', { name: '展开 req-90 的明细', exact: true })).toHaveCount(0)
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
      expect((await model.boundingBox())!.width).toBe(256)
      expect((await request.boundingBox())!.width).toBe(320)
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
  await page.getByRole('dialog').getByRole('button', { name: '关闭', exact: true }).click()
  const [download] = await Promise.all([page.waitForEvent('download'), page.getByRole('button', { name: '导出本页 CSV', exact: true }).click()])
  const csv = await readFile((await download.path())!, 'utf8')
  const lines = csv.trim().split('\n')
  expect(lines[0]).toContain('upstream_request_id,node,key_name,key_prefix,requested_model')
  expect(lines[0]).toContain('cache_write_5m_tokens,cache_write_1h_tokens')
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
  test(`日志调用对象 ${width}px：详情抽屉内对象不溢出，横向看列不会把明细移走`, async ({ page }) => {
    await prepare(page)
    await page.setViewportSize({ width, height: 800 })
    await page.goto('/admin/logs')
    await page.getByRole('button', { name: '展开 req-20 的明细', exact: true }).click()
    const identities = page.getByLabel('调用对象', { exact: true })
    const wrapper = page.getByRole('table').locator('..')
    await identities.scrollIntoViewIfNeeded()
    const viewport = await page.getByRole('dialog').boundingBox()
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
    const controls = await collapse.getAttribute('aria-controls')
    await expect(page.locator(`[id="${controls}"]`)).toBeVisible()
    await expect(page.getByText('req-20', { exact: true })).toBeVisible()
    await expect(page.getByRole('dialog', { name: '请求与账单详情' })).toBeVisible()
    await page.keyboard.press('Escape')
    await expect(expand).toBeFocused()
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

for (const path of ['/portal/logs', '/admin/logs']) {
  for (const sample of [
    { name: '非流式', is_stream: false, latency_ms: 14700, ttft_ms: null, average: '34 tok/s', generation: '—' },
    { name: '流式', is_stream: true, latency_ms: 14700, ttft_ms: 700, average: '34 tok/s', generation: '35.7 tok/s' },
    { name: '缺少首字', is_stream: true, latency_ms: 14700, ttft_ms: null, average: '34 tok/s', generation: '—' },
    { name: '无有效耗时', is_stream: false, latency_ms: 0, ttft_ms: null, average: '—', generation: '—' },
  ]) {
    test(`日志速度 ${path} ${sample.name}：平均输出速度与流式生成速度分别显示`, async ({ page }) => {
      await prepare(page)
      const row = { ...detailedLog, is_stream: sample.is_stream, latency_ms: sample.latency_ms, ttft_ms: sample.ttft_ms }
      const endpoint = path === '/admin/logs' ? '**/admin/logs?*' : '**/api/me/logs?*'
      await page.route(endpoint, (route) => route.fulfill({ json: { data: [row], next_before: null } }))
      await page.goto(path)
      const rate = page.locator('[data-slot="output-throughput"]')
      await expect(rate).toHaveText(sample.average)
      await expect(rate).toHaveAccessibleName(`输出吞吐量 ${sample.average}`)
      await rate.focus()
      await expect(page.getByRole('tooltip')).toContainText('包含首字等待，不是纯生成速度')
      // Collapsed row borders can round the first/last row up by one pixel.
      expect(Math.abs(await page.locator('tbody tr').first().evaluate(node => node.offsetHeight) - 44)).toBeLessThanOrEqual(1)
      await page.getByRole('button', { name: '展开 req-20 的明细', exact: true }).click()
      const dialog = page.getByRole('dialog')
      const metric = (label: string) => dialog.locator('dt').filter({ hasText: new RegExp(`^${label}$`) }).locator('..').locator('dd')
      await expect(metric('平均输出速度')).toHaveText(sample.average)
      await expect(metric('生成速度（估算）')).toHaveText(sample.generation)
      if (!sample.is_stream) await expect(metric('首字')).toHaveText('不适用')
    })
  }
}

test('输出吞吐量：严格配对计量，零输出与缺失分开，不以模型或缓存推测速度', () => {
  expect(outputRates(detailedLog)).toEqual({ average: 625, generation: 500 * 1000 / 700 })
  expect(outputRates({ ...detailedLog, is_stream: false })).toEqual({ average: 625, generation: null })
  expect(outputRates({ ...detailedLog, ttft_ms: null })).toEqual({ average: 625, generation: null })
  expect(outputRates({ ...detailedLog, ttft_ms: 0 })).toEqual({ average: 625, generation: 625 })
  expect(outputRates({ ...detailedLog, ttft_ms: 800 })).toEqual({ average: 625, generation: null })
  expect(outputRates({ ...detailedLog, usage: { ...detailedLog.usage, completion_tokens: 0 } })).toEqual({ average: 0, generation: null })
  for (const latency_ms of [null, 0, -1, Infinity, NaN]) expect(outputRates({ ...detailedLog, latency_ms })).toEqual({ average: null, generation: null })
  for (const completion_tokens of [-1, NaN, Infinity, 0.5]) expect(outputRates({ ...detailedLog, usage: { ...detailedLog.usage, completion_tokens } })).toEqual({ average: null, generation: null })
  for (const input_unit of [null, undefined, 'characters', 'unknown']) expect(outputRates({ ...detailedLog, usage: { ...detailedLog.usage, input_unit } })).toEqual({ average: null, generation: null })
  expect(outputRates({ ...detailedLog, usage: { ...detailedLog.usage, input_characters: 100 } })).toEqual({ average: null, generation: null })
  expect(formatOutputRate(0, 'en')).toBe('0 tok/s')
  expect(formatOutputRate(null, 'en')).toBe('—')
})

for (const path of ['/portal/logs?scope=user', '/admin/logs']) {
  test(`输出吞吐量 ${path}：列表区分已测零、未知单位、字符与缺失耗时`, async ({ page }) => {
    await prepare(page)
    await page.setViewportSize({ width: 1920, height: 1000 })
    const rows = [
      detailedLog,
      { ...detailedLog, usage: { ...detailedLog.usage, completion_tokens: 0 } },
      { ...detailedLog, usage: { ...detailedLog.usage, input_unit: null } },
      { ...detailedLog, usage: { ...detailedLog.usage, input_unit: 'characters', input_characters: 100 } },
      { ...detailedLog, latency_ms: null },
    ].map((row, i) => ({ ...row, id: 20 - i, request_id: `req-${20 - i}` }))
    await page.route('**/api/me/logs?*', route => route.fulfill({ json: { data: rows, next_before: null } }))
    await page.route('**/admin/logs?*', route => route.request().isNavigationRequest() ? route.fallback() : route.fulfill({ json: { data: rows } }))
    await page.goto(path)
    const rates = page.locator('[data-slot="output-throughput"]')
    await expect(rates).toHaveText(['625 tok/s', '0 tok/s', '—', '—', '—'])
    for (const index of [0, 1]) await expect(rates.nth(index)).toHaveAttribute('data-state', 'measured')
    for (const index of [2, 3, 4]) await expect(rates.nth(index)).toHaveAttribute('data-state', 'missing')
    for (const row of await page.locator('tbody tr').all()) expect(Math.abs(await row.evaluate(node => node.offsetHeight) - 44)).toBeLessThanOrEqual(1)
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    await rates.nth(2).focus()
    await expect(page.getByRole('tooltip')).toContainText('暂无有效的 Token 计量或耗时数据')
    await page.screenshot({ path: `test-results/log-throughput-${path.startsWith('/admin') ? 'admin' : 'personal'}.png`, animations: 'disabled' })
    await page.getByRole('button', { name: '展开 req-18 的明细', exact: true }).click()
    const performance = page.getByRole('dialog').locator('section').filter({ has: page.getByRole('heading', { name: '响应速度', exact: true }) })
    await expect(performance.locator('dt').getByText('平均输出速度', { exact: true }).locator('..').locator('dd')).toHaveText('—')
  })
}

for (const path of ['/portal/logs?scope=user', '/admin/logs']) {
  test(`诊断抽屉 ${path}：失败与退款并存，错误原因和长ID完整可见`, async ({ page }) => {
    await prepare(page)
    await page.setViewportSize({ width: 390, height: 844 })
    const upstreamId = `upstream-${'very-long-correlation-id-'.repeat(12)}`
    const diagnostics = { error_phase: 'upstream', error_message: 'upstream quota exceeded', request_failed: true,
      response_model: 'actual-provider-model', reasoning_effort: 'high',
      ...(path.startsWith('/admin') ? { user_agent: 'sdk/2', session_id: 'client-session', attempts: [
        { channel_id: 1, channel_key_id: 2, provider: 'openai', upstream_model: 'provider-model', upstream_endpoint: '/v1/responses', status: 429, outcome: 'failure', error_message: 'quota exceeded', duration_ms: 100 },
        { channel_id: 3, channel_key_id: 4, provider: 'openai', upstream_model: 'provider-model', upstream_endpoint: '/v1/responses', status: 503, outcome: 'failure', error_message: 'temporarily unavailable', duration_ms: 200 },
      ] } : {}),
    }
    const row = { ...detailedLog, status: 30, log_type: 5, is_error: true, error_code: 'upstream_status', upstream_request_id: upstreamId, diagnostics }
    await page.route('**/api/me/logs?*', route => route.fulfill({ json: { data: [row], next_before: null } }))
    await page.route('**/admin/logs?*', route => route.request().isNavigationRequest() ? route.fallback() : route.fulfill({ json: { data: [row] } }))
    await page.goto(path)
    if (path.startsWith('/portal')) {
      const table = page.getByRole('table')
      await expect(table).toContainText('失败')
      await expect(table).toContainText('已退款')
    }
    await page.getByRole('button', { name: '展开 req-20 的明细' }).click()
    const drawer = page.getByRole('dialog')
    await expect(drawer).toContainText('upstream quota exceeded')
    await expect(drawer).toContainText('actual-provider-model')
    await expect(drawer).toContainText('model-alias → gpt-alpha → actual-provider-model')
    const id = drawer.getByText(upstreamId, { exact: true })
    await expect(id).toHaveText(upstreamId)
    expect(await id.evaluate(el => el.scrollWidth <= el.clientWidth)).toBe(true)
    if (path.startsWith('/admin')) {
      await expect(drawer.locator('ol li')).toHaveCount(2)
      await expect(drawer).toContainText('temporarily unavailable')
      await expect(drawer).toContainText('client-session')
    } else await expect(drawer.locator('ol li')).toHaveCount(0)
    expect(await drawer.evaluate(el => el.scrollWidth <= el.clientWidth)).toBe(true)
    await drawer.getByText('请求诊断', { exact: true }).scrollIntoViewIfNeeded()
    await page.screenshot({ path: `test-results/log-diagnostics-${path.startsWith('/admin') ? 'admin' : 'personal'}-mobile.png`, animations: 'disabled' })
  })
}

for (const timing of [null, 0]) {
  test(`管理员耗时 ${timing === null ? '缺失' : '真实零'}：不推算缺失首字`, async ({ page }) => {
    await prepare(page)
    await page.route('**/admin/logs?*', route => route.request().isNavigationRequest() ? route.fallback() : route.fulfill({ json: { data: [{ ...log, ttft_ms: timing }] } }))
    await page.goto('/admin/logs')
    await page.getByRole('button', { name: '展开 req-20 的明细' }).click()
    const performance = page.getByRole('dialog').locator('section').filter({ has: page.getByRole('heading', { name: '响应速度', exact: true }) })
    await expect(performance.locator('dt').getByText('首字', { exact: true }).locator('..').locator('dd')).toHaveText(timing === null ? '—' : '0 ms')
    await expect(performance.locator('dt').getByText('生成速度（估算）', { exact: true }).locator('..').locator('dd')).toHaveText(timing === null ? '—' : '625 tok/s')
  })
}

for (const path of ['/portal/logs?scope=user', '/admin/logs']) {
  test(`字符计价 ${path}：展示字符数量和快照计费，隐藏Token速度`, async ({ page }) => {
    await prepare(page)
    const snapshot = { ...detailedLog.pricing_snapshot, input_unit: 'characters', input_characters: 1000, final_unit_price_input_per_1m_usd: '15' }
    const row = { ...detailedLog, endpoint: '/v1/audio/speech', ratio_snapshot: JSON.stringify(snapshot), pricing_snapshot: snapshot,
      usage: { prompt_tokens: 0, cached_tokens: 0, completion_tokens: 0, reasoning_tokens: 0, input_unit: 'characters', input_characters: 1000 } }
    expect(billingLines(row)).toEqual([{ name: 'inputCharacters', quantity: 1000, unitMicro: 15000000, amountMicro: 15000 }])
    await page.route('**/api/me/logs?*', route => route.fulfill({ json: { data: [row], next_before: null } }))
    await page.route('**/admin/logs?*', route => route.request().isNavigationRequest() ? route.fallback() : route.fulfill({ json: { data: [row] } }))
    await page.goto(path)
    await expect(page.locator('[data-slot="log-character-usage"]')).toContainText('1,000')
    await page.getByRole('button', { name: '展开 req-20 的明细' }).click()
    const drawer = page.getByRole('dialog')
    await expect(drawer.getByRole('table', { name: '快照计费分项' })).toContainText('输入字符')
    await expect(drawer.getByRole('table', { name: '快照计费分项' })).toContainText('单价 / 百万字符')
    await expect(drawer).not.toContainText('0 tok/s')
  })
}

test('新增高级筛选：URL和后端查询保留分组、客户端、日志类型及上游ID', async ({ page }) => {
  const queries = await prepare(page)
  await page.goto('/admin/logs?group=vip&client_type=codex&log_type=5&upstream_request_id=upstream-123')
  await expect(page.getByLabel('分组', { exact: true })).toHaveValue('vip')
  await expect(page.getByLabel('客户端类型', { exact: true })).toHaveValue('codex')
  await expect(page.getByLabel('上游请求 ID', { exact: true })).toHaveValue('upstream-123')
  await expect.poll(() => queries.some(url => url.pathname === '/admin/logs'
    && url.searchParams.get('group') === 'vip' && url.searchParams.get('client_type') === 'codex'
    && url.searchParams.get('log_type') === '5' && url.searchParams.get('upstream_request_id') === 'upstream-123')).toBe(true)
  await page.reload()
  await expect(page.getByLabel('分组', { exact: true })).toHaveValue('vip')
})
