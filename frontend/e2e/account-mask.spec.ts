import { expect, test } from '@playwright/test'
import type { Page } from '@playwright/test'
import { readdirSync, readFileSync } from 'node:fs'
import { join, relative } from 'node:path'
import { fileURLToPath } from 'node:url'
import { flowIdentity } from '../src/features/analytics/flow-labels'
import { trendChart } from '../src/features/analytics/trend-data'
import type { FlowNode, TrendResp } from '../src/features/analytics/types'
import { maskAccount, maskChannelName, maskEmails } from '../src/lib/mask'

// 订阅账号（OAuth 登录的邮箱）展示时遮住中间部分，渠道名只有被识别为账号名才遮，其余原样。

test('maskEmails：只遮邮箱的用户名，头尾各留一两个字符，中间固定三个星号；域名与普通文字原样', () => {
  expect(maskEmails('laird_phelpsdev@example.com')).toBe('la***ev@example.com')
  expect(maskEmails('alice.dev@gmail.com')).toBe('al***ev@gmail.com')
  expect(maskEmails('carol@outlook.com')).toBe('c***l@outlook.com')
  expect(maskEmails('bob@hot-shot.com')).toBe('b***@hot-shot.com')
  expect(maskEmails('a@x.io')).toBe('a***@x.io')
  // 不管原来多长都是三个星号：长度不泄漏
  expect(maskEmails('a-very-long-account-name@example.com')).toBe('a-***me@example.com')
  // 一段文字里的每个邮箱都遮，其余原样
  expect(maskEmails('渠道 alice.dev@gmail.com 与 bob@hot-shot.com 失败')).toBe('渠道 al***ev@gmail.com 与 b***@hot-shot.com 失败')
  // 普通名字不是账号，不动
  for (const plain of ['openai-main', 'spare-compat', 'OpenAI Primary', '', '#12', 'user@']) expect(maskEmails(plain)).toBe(plain)
  // 幂等：遮过的再遮一次不再变（展示层不小心遮两遍也安全）
  expect(maskEmails(maskEmails('alice.dev@gmail.com'))).toBe('al***ev@gmail.com')
  expect(maskAccount(maskAccount('alice.dev@gmail.com'))).toBe('al***ev@gmail.com')
  expect(maskAccount(maskAccount('acct_0123456789abcdef'))).toBe('ac***ef')
})

test('maskAccount：邮箱按邮箱遮，不是邮箱的账号标识整串遮中间', () => {
  expect(maskAccount('bob@hot-shot.com')).toBe('b***@hot-shot.com')
  expect(maskAccount('acct_0123456789abcdef')).toBe('ac***ef')
  expect(maskAccount('dave')).toBe('d***e')
  expect(maskAccount('ab')).toBe('a***')
  expect(maskAccount('')).toBe('')
  // 按字符而不是 UTF-16 单元：中文、emoji 不会被劈成两半
  expect(maskAccount('张三的订阅账号')).toBe('张***号')
  expect(maskAccount('😀😀😀😀😀😀😀😀')).toBe('😀😀***😀😀')
})

test('maskChannelName：渠道名是账号名（邮箱，或等于自己某把 key 的账号标识）才遮，其余原样', () => {
  // 邮箱形的渠道名：遮
  expect(maskChannelName('alice.dev@gmail.com')).toBe('al***ev@gmail.com')
  // 名字等于账号标识（账号不是邮箱）：遮；大小写与首尾空格不影响识别
  expect(maskChannelName('dave-claude-pro', ['dave-claude-pro'])).toBe('da***ro')
  expect(maskChannelName(' Dave-Claude-Pro ', ['dave-claude-pro'])).toBe(maskAccount(' Dave-Claude-Pro '))
  // 名字只是普通名字：原样，哪怕它的 key 记录了别的账号
  expect(maskChannelName('team-shared', ['erin@hot-shot.com'])).toBe('team-shared')
  expect(maskChannelName('openai-main', [undefined, null])).toBe('openai-main')
  expect(maskChannelName('', ['x'])).toBe('')
})

test('分析页的渠道维度：流向图节点名与按渠道堆叠的图例遮邮箱，其他维度与普通渠道名不动', () => {
  const t = ((key: string) => key) as never
  const node = (over: Partial<FlowNode>) => ({ id: 'n', stage: 'channel', key: '7', label: 'ivan.k@gmail.com', value: 1, entity_status: 'active', ...over }) as FlowNode
  expect(flowIdentity(node({}), t).primary).toBe('i***k@gmail.com')
  expect(flowIdentity(node({ label: 'plain-channel' }), t).primary).toBe('plain-channel')
  // 用户节点不是订阅账号：邮箱形的用户名原样（用户页本来就有邮箱列）
  expect(flowIdentity(node({ stage: 'user', label: 'ivan.k@gmail.com' }), t).primary).toBe('ivan.k@gmail.com')

  const resp = (stack: string): TrendResp => ({
    days: 1, granularity: 'day', stack,
    series: [{ key: '7', label: 'ivan.k@gmail.com' }, { key: '8', label: 'plain-channel' }],
    window: { start_at: '2026-09-20 00:00:00', end_at: '2026-09-20 23:00:00', timezone: 'UTC', generated_at: '2026-09-20 12:00:00' },
    data: [{ bucket: '2026-09-20', values: { '7': { requests: 3 }, '8': { requests: 1 } } }],
  }) as unknown as TrendResp
  const labels = (stack: string) => trendChart(resp(stack), 'requests', 'label', 'ttft', 'other', 'unknown').series.map((series) => series.label)
  expect(labels('channel')).toEqual(['i***k@gmail.com', 'plain-channel'])
  expect(labels('user')).toEqual(['ivan.k@gmail.com', 'plain-channel'])
})

async function prepare(page: Page) {
  await page.addInitScript(() => {
    localStorage.setItem('okapi.key', 'mask-fixture')
    localStorage.setItem('okapi.lang', 'zh-CN')
  })
  await page.emulateMedia({ reducedMotion: 'reduce' })
  const row = (id: number, channel: string) => ({
    id, request_id: `req-${id}`, upstream_request_id: `up-${id}`, model: 'gpt-alpha', log_type: 2, status: 20,
    user_id: 1, username: 'alice', api_key_id: 1, key_name: 'app-key', key_prefix: 'sk-prefix', channel_id: id, channel_name: channel, channel_key_id: 2,
    provider: 'anthropic_max', client_type: 'sdk', client_ip: '203.0.113.1', node: 'edge-west', group: 'default',
    usage: { prompt_tokens: 1000, cached_tokens: 100, completion_tokens: 500, reasoning_tokens: 50, input_unit: 'tokens' },
    amount_micro: 20000, original_amount_micro: 25000, discount_micro: 5000, upstream_cost_micro: 10000,
    pricing_snapshot: null, error_code: null, latency_ms: 800, ttft_ms: 100, is_stream: true,
    ts: '2026-09-26 12:00:00', created_at: '2026-09-26T12:00:00Z', retry_count: 0, failover_count: 0,
    sticky_layer: 0, upstream_status: 200, is_error: false, ratio_snapshot: '',
  })
  await page.route('**/*', (route) => {
    const request = route.request(), url = new URL(request.url())
    if (request.isNavigationRequest()) return route.fulfill({ path: fileURLToPath(new URL('../dist/index.html', import.meta.url)), contentType: 'text/html' })
    if (!/^\/(api|admin|auth)\//.test(url.pathname)) return route.continue()
    const json = url.pathname === '/api/me' ? { user_id: 1, key_id: 1, role: 100, permissions: ['*'], balance_micro: 0, group: 'default' }
      : url.pathname === '/api/notice' ? { notice: null }
        : url.pathname === '/admin/logs' ? { data: [row(1, 'grace.hopper@gmail.com'), row(2, 'openai-main')], scope: { channel: { id: 1, name: 'grace.hopper@gmail.com', provider: 'anthropic_max' } } }
          : { data: [], total: 0 }
    return route.fulfill({ json })
  })
}

test('管理端日志：渠道列与展开明细里邮箱形的渠道名遮住中间，普通渠道名原样', async ({ page }) => {
  await prepare(page)
  await page.goto('/admin/logs')
  await expect(page.getByRole('cell', { name: 'gr***er@gmail.com', exact: true })).toBeVisible()
  await expect(page.getByRole('cell', { name: 'openai-main', exact: true })).toBeVisible()
  await page.getByRole('button', { name: '展开 req-1 的明细', exact: true }).click()
  const detail = page.getByRole('dialog')
  await expect(detail.getByText('gr***er@gmail.com', { exact: true })).toBeVisible()
  expect(await page.locator('body').innerText()).not.toContain('grace.hopper@gmail.com')
  expect(await page.locator('body').innerHTML()).not.toContain('grace.hopper@gmail.com')
})

test('管理端日志的渠道筛选：已选渠道回填到输入框与提示句里的名字也是遮过的', async ({ page }) => {
  await prepare(page)
  await page.goto('/admin/logs?channel_id=1')
  await expect(page.getByText('已选择 gr***er@gmail.com · ID 1', { exact: true })).toBeVisible()
  expect(await page.locator('body').innerText()).not.toContain('grace.hopper@gmail.com')
  expect(await page.locator('input').evaluateAll((nodes) => nodes.map((node) => (node as HTMLInputElement).value).join('|'))).not.toContain('grace.hopper@gmail.com')
})

test('显示渠道名 / 订阅账号的源码都经过遮罩：引用 channel_name / top_channel_name / account_label 的文件必须引入 lib/mask', () => {
  const root = fileURLToPath(new URL('../src/', import.meta.url))
  const files: string[] = []
  const walk = (dir: string) => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const path = join(dir, entry.name)
      if (entry.isDirectory()) walk(path)
      else if (/\.tsx?$/.test(entry.name) && entry.name !== 'types.ts' && entry.name !== 'mask.ts') files.push(path)
    }
  }
  walk(root)
  const unmasked = files
    .filter((file) => /\b(channel_name|top_channel_name|account_label)\b/.test(readFileSync(file, 'utf8')))
    .filter((file) => !readFileSync(file, 'utf8').includes('@/lib/mask'))
    .map((file) => relative(root, file).replaceAll('\\', '/'))
  // 新增显示渠道名 / 账号的地方，请用 maskChannelName / maskEmails / maskAccount 包一层（lib/mask.ts）
  expect(unmasked).toEqual([])
})
