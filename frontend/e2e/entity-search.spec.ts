import { expect, test } from '@playwright/test'
import type { Page } from '@playwright/test'
import { fileURLToPath } from 'node:url'

const users = [{ id: 7, username: 'alice', email: 'alice@example.test' }, { id: 42, username: 'bob', email: 'bob@example.test' }]
const keys = [{ id: 10, name: 'production', user_id: 7, username: 'alice', key_prefix: 'sk-alice' }, { id: 20, name: 'production', user_id: 42, username: 'bob', key_prefix: 'sk-bob' }]
const channels = [{ id: 9, name: 'OpenAI Primary', provider: 'openai', api_base: 'https://gateway.example.test/v1' }]
const directoryPaths = ['/admin/users', '/admin/keys', '/admin/channels']

async function prepare(page: Page, permissions = ['*']) {
  const requests: URL[] = []
  await page.addInitScript(() => { localStorage.setItem('okapi.key', 'entity-search-fixture'); localStorage.setItem('okapi.lang', 'zh-CN') })
  await page.route('**/*', (route) => {
    const request = route.request(), url = new URL(request.url())
    if (request.isNavigationRequest()) return route.fulfill({ path: fileURLToPath(new URL('../dist/index.html', import.meta.url)), contentType: 'text/html' })
    if (!/^\/(api|admin|auth)\//.test(url.pathname)) return route.continue()
    expect(request.method()).toBe('GET')
    requests.push(url)
    const p = url.searchParams, term = (p.get('q') ?? '').toLowerCase()
    const owner = users.find((u) => u.id === Number(p.get('user_id') ?? 7))
    const key = keys.find((k) => k.id === Number(p.get('api_key_id') ?? 10))
    const channel = channels.find((c) => c.id === Number(p.get('channel_id') ?? 9))
    const scope = {
      user: p.has('user_id') ? { id: Number(p.get('user_id')), username: owner?.username ?? null } : undefined,
      api_key: p.has('api_key_id') ? { id: Number(p.get('api_key_id')), name: key?.name ?? null, key_prefix: key?.key_prefix ?? null, user_id: key?.user_id ?? null, username: key?.username ?? null } : undefined,
      channel: p.has('channel_id') ? { id: Number(p.get('channel_id')), name: channel?.name ?? null, provider: channel?.provider ?? null } : undefined,
    }
    const found = url.pathname === '/admin/users' ? users.filter((u) => `${u.username} ${u.email}`.includes(term))
      : url.pathname === '/admin/keys' ? keys.filter((k) => `${k.name} ${k.username}`.includes(term))
        : channels.filter((c) => `${c.name} ${c.api_base}`.toLowerCase().includes(term))
    const fixtures: Record<string, unknown> = {
      '/api/me': { user_id: 1, key_id: 1, role: 100, permissions, balance_micro: 10000000, group: 'default' },
      '/api/notice': { notice: null },
      '/admin/stats/trend': { days: 7, granularity: 'day', scope, total: { requests: 100, tokens: 10000, amount_micro: 1000000, errors: 0, error_rate_bp: 0 }, previous: {}, data: [] },
      '/admin/logs/stat': { requests: 1, errors: 0, error_rate_bp: 0, tokens: 1500, amount_micro: 20000, discount_micro: 0, users: 1, cache_hit_bp: 1000, cached_tokens: 100, rpm: 1, tpm: 1500, rate_source: 'clickhouse' },
      '/admin/logs': { scope, data: [{
        ts: '2026-09-26 12:00:00', request_id: 'entity-log', upstream_request_id: '', user_id: owner?.id ?? Number(p.get('user_id') ?? 7), username: owner?.username ?? '',
        api_key_id: key?.id ?? 10, key_name: key?.name ?? '', key_prefix: key?.key_prefix ?? '', model: 'gpt-alpha', channel_id: channel?.id ?? Number(p.get('channel_id') ?? 9), channel_name: channel?.name ?? '', provider: channel?.provider ?? '',
        usage: { prompt_tokens: 1000, cached_tokens: 100, completion_tokens: 500, reasoning_tokens: 0 },
        amount_micro: 20000, original_amount_micro: 20000, discount_micro: 0, upstream_cost_micro: 10000, latency_ms: 800, ttft_ms: 100, is_stream: false,
        error_code: '', is_error: false, group: 'default', client_type: 'sdk', retry_count: 0, failover_count: 0,
      }] },
    }
    return route.fulfill({ json: directoryPaths.includes(url.pathname) ? { total: found.length, data: found } : fixtures[url.pathname] ?? { data: [] } })
  })
  return requests
}

test('分析按用户邮箱、渠道地址和同名密钥检索，选择只改草稿，应用准确 ID 且刷新可读', async ({ page }) => {
  const requests = await prepare(page)
  await page.goto('/admin/stats?days=30')
  const quick = page.getByRole('region', { name: '快速筛选', exact: true })
  const dimension = quick.getByRole('combobox', { name: '筛选维度' })
  const cases = [
    { dim: 'user_id', label: '用户', term: 'bob@example.test', option: /bob.*ID 42/, name: 'bob', id: '42' },
    { dim: 'channel_id', label: '渠道', term: 'gateway.example.test', option: /OpenAI Primary.*ID 9/, name: 'OpenAI Primary', id: '9' },
    { dim: 'api_key_id', label: '密钥', term: 'production', option: /production.*ID 20.*bob.*sk-bob/, name: 'production', id: '20' },
  ]
  for (const item of cases) {
    await dimension.selectOption(item.dim)
    const input = quick.getByRole('combobox', { name: item.label, exact: true })
    const before = requests.filter((url) => url.pathname === '/admin/stats/trend').length
    await input.fill(item.term)
    await expect(input).not.toHaveAttribute('aria-invalid', 'true')
    await expect(quick.getByRole('button', { name: '添加过滤' })).toBeDisabled()
    await page.getByRole('option', { name: item.option }).click()
    await expect(input).toHaveValue(item.name)
    expect(requests.filter((url) => url.pathname === '/admin/stats/trend')).toHaveLength(before)
    await quick.getByRole('button', { name: '添加过滤' }).click()
    await expect.poll(() => requests.filter((url) => url.pathname === '/admin/stats/trend').at(-1)?.searchParams.get(item.dim)).toBe(item.id)
    await expect(quick).toContainText(item.name)
  }
  await page.reload()
  await expect(quick.getByRole('button', { name: '移除过滤 bob', exact: true })).toBeVisible()
  await expect(quick.getByRole('button', { name: /移除过滤 production.*sk-bob/ })).toBeVisible()
  await expect(quick.getByRole('button', { name: '移除过滤 OpenAI Primary', exact: true })).toBeVisible()
  await expect(page).toHaveURL(/days=30/)
  const searches = requests.filter((url) => directoryPaths.includes(url.pathname))
  expect(searches.length).toBeGreaterThanOrEqual(3)
  for (const url of searches) {
    expect(url.searchParams.get('limit')).toBe('20')
    expect(url.searchParams.get('offset')).toBe('0')
  }
})

test('切换维度丢弃迟到的渠道候选，数字输入不被已知名称替换，空结果和故障可用历史 ID', async ({ page }) => {
  const requests = await prepare(page)
  let release: () => void = () => undefined
  const pending = new Promise<void>((resolve) => { release = resolve })
  let started = false
  await page.route('**/admin/channels?*', async (route) => {
    const query = new URL(route.request().url()).searchParams.get('q')
    if (query !== 'slow') return route.fallback()
    started = true
    await pending
    return route.fulfill({ json: { total: 1, data: [{ id: 42, name: 'slow channel', provider: 'openai' }] } })
  })
  await page.goto('/admin/stats?user_id=7')
  const quick = page.getByRole('region', { name: '快速筛选', exact: true })
  const dimension = quick.getByRole('combobox', { name: '筛选维度' })
  await dimension.selectOption('channel_id')
  await quick.getByRole('combobox', { name: '渠道', exact: true }).fill('slow')
  await expect.poll(() => started).toBe(true)
  await dimension.selectOption('user_id')
  const user = quick.getByRole('combobox', { name: '用户', exact: true })
  await user.fill('bob')
  await expect(page.getByRole('option', { name: /bob.*ID 42/ })).toBeVisible()
  release()
  await expect(page.getByRole('option', { name: /slow channel/ })).toHaveCount(0)
  await user.fill('')
  await user.pressSequentially('777')
  await expect(user).toHaveValue('777')
  await user.press('Enter')
  await expect(page).toHaveURL(/user_id=777/)
  await expect(quick.getByRole('button', { name: '移除过滤 ID 777', exact: true })).toBeVisible()
  await expect(quick.getByRole('button', { name: '移除过滤 alice', exact: true })).toHaveCount(0)
  await dimension.selectOption('channel_id')
  const channel = quick.getByRole('combobox', { name: '渠道', exact: true })
  await channel.fill('no-matches')
  await expect(page.getByText('没有匹配结果，请换个关键词，或直接输入 ID。')).toBeVisible()
  await page.route('**/admin/channels?*', (route) => route.fulfill({ status: 500, json: { error: { code: 'internal_error' } } }))
  await channel.fill('unavailable')
  await expect(page.getByText('暂时无法加载候选，可直接输入 ID。')).toBeVisible()
  const before = requests.filter((url) => url.pathname === '/admin/stats/trend').length
  await channel.fill('1e2')
  await expect(channel).toHaveAttribute('aria-invalid', 'true')
  await channel.press('Enter')
  expect(requests.filter((url) => url.pathname === '/admin/stats/trend')).toHaveLength(before)
  await channel.fill('999')
  await channel.press('Enter')
  await expect(page).toHaveURL(/channel_id=999/)
})

test('日志三类候选按名称选择，查询时回首页，刷新复用日志内的名称，后退恢复条件', async ({ page }) => {
  const requests = await prepare(page)
  await page.goto('/admin/logs?page=3')
  await page.getByText('更多筛选', { exact: true }).click()
  const user = page.locator('#lf-user_id'), key = page.locator('#lf-api_key_id'), channel = page.locator('#lf-channel_id')
  await user.fill('bob@example.test')
  await page.getByRole('option', { name: /bob.*ID 42/ }).click()
  await key.fill('production')
  await expect(page.getByRole('listbox').getByRole('option')).toHaveCount(2)
  await page.getByRole('option', { name: /ID 20.*bob.*sk-bob/ }).click()
  await channel.fill('gateway.example.test')
  await expect(page.getByRole('option', { name: /OpenAI Primary.*ID 9/ })).toBeVisible()
  await channel.press('ArrowDown')
  await channel.press('Enter')
  expect(requests.filter((url) => url.pathname === '/admin/logs')).toHaveLength(1)
  await expect(channel).toHaveValue('OpenAI Primary')
  await page.getByRole('button', { name: '搜索', exact: true }).click()
  await expect(page).toHaveURL(/user_id=42/)
  await expect(page).toHaveURL(/api_key_id=20/)
  await expect(page).toHaveURL(/channel_id=9/)
  await expect(page).not.toHaveURL(/page=/)
  await page.reload()
  await expect(user).toHaveValue('bob')
  await expect(channel).toHaveValue('OpenAI Primary')
  await expect(key).toHaveValue('production')
  await page.getByRole('button', { name: '清空筛选', exact: true }).click()
  const before = requests.filter((url) => url.pathname === '/admin/logs').length
  await expect(user).toHaveValue('')
  expect(requests.filter((url) => url.pathname === '/admin/logs')).toHaveLength(before)
  await page.getByRole('button', { name: '搜索', exact: true }).click()
  await expect(page).not.toHaveURL(/user_id=/)
  await page.goBack()
  await expect(user).toHaveValue('bob')
  await expect(channel).toHaveValue('OpenAI Primary')
  await expect(key).toHaveValue('production')
  expect(requests.some((url) => /\/admin\/(users|keys|channels)\/\d/.test(url.pathname))).toBe(false)
})

test('空日志仍回填全部筛选名称；刷新不查目录，错误对象的 scope 不套到当前 ID', async ({ page }) => {
  const requests = await prepare(page, ['billing.read'])
  await page.route('**/admin/logs?*', (route) => {
    if (route.request().isNavigationRequest()) return route.fallback()
    requests.push(new URL(route.request().url()))
    return route.fulfill({ json: { data: [], scope: {
      user: { id: 42, username: 'bob' },
      api_key: { id: 20, name: 'production', key_prefix: 'sk-bob', user_id: 42, username: 'bob' },
      channel: { id: 9, name: 'OpenAI Primary', provider: 'openai' },
    } } })
  })
  await page.goto('/admin/logs?user_id=42&api_key_id=20&channel_id=9')
  const user = page.locator('#lf-user_id'), key = page.locator('#lf-api_key_id'), channel = page.locator('#lf-channel_id')
  for (const refresh of [false, true]) {
    if (refresh) await page.reload()
    await expect(page.getByText('当前窗口与过滤条件下没有日志。放宽时间窗或清空过滤再试。')).toBeVisible()
    await expect(user).toHaveValue('bob')
    await expect(key).toHaveValue('production')
    await expect(channel).toHaveValue('OpenAI Primary')
  }
  expect(requests.filter((url) => directoryPaths.includes(url.pathname))).toHaveLength(0)
  await page.route('**/admin/logs?*', (route) => route.request().isNavigationRequest() ? route.fallback() : route.fulfill({ json: { data: [], scope: {
    user: { id: 42, username: '' }, api_key: { id: 20, name: '', key_prefix: 'sk-bob' }, channel: { id: 9, name: '' },
  } } }))
  await page.reload()
  await expect(user).toHaveValue('未命名用户')
  await expect(key).toHaveValue('未命名密钥')
  await expect(channel).toHaveValue('未命名渠道')
  await page.goto('/admin/logs?user_id=999&api_key_id=999&channel_id=999')
  await expect(user).toHaveValue('999')
  await expect(key).toHaveValue('999')
  await expect(channel).toHaveValue('999')
  await expect(page.getByText('已选择 production · ID 20')).toHaveCount(0)
})

test('只读报表账号不请求无权限目录，已有日志名称仍可选，历史 ID 可手输', async ({ page }) => {
  const requests = await prepare(page, ['billing.read'])
  await page.goto('/admin/logs')
  await page.getByText('更多筛选', { exact: true }).click()
  const user = page.locator('#lf-user_id')
  await user.fill('alice')
  await page.getByRole('option', { name: /alice.*ID 7/ }).click()
  await page.locator('#lf-api_key_id').fill('321')
  await page.locator('#lf-channel_id').fill('999')
  await page.getByRole('button', { name: '搜索', exact: true }).click()
  await expect(page).toHaveURL(/user_id=7/)
  await expect(page).toHaveURL(/api_key_id=321/)
  await expect(page).toHaveURL(/channel_id=999/)
  expect(requests.filter((url) => directoryPaths.includes(url.pathname))).toHaveLength(0)
})

for (const width of [320, 390, 1280]) {
  test(`日志名称筛选 ${width}px：候选跟随输入框，长名称不溢出，中文输入不提前提交`, async ({ page }) => {
    const requests = await prepare(page)
    const longName = 'production-数据分析服务-专用访问密钥-只读监控及历史用量'
    await page.route('**/admin/keys?*', (route) => {
      requests.push(new URL(route.request().url()))
      return route.fulfill({ json: { total: 2, data: keys.map((key) => ({ ...key, name: longName })) } })
    })
    await page.setViewportSize({ width, height: 800 })
    await page.goto('/admin/logs')
    await page.getByText('更多筛选', { exact: true }).click()
    const key = page.locator('#lf-api_key_id')
    await key.dispatchEvent('compositionstart')
    await key.fill('生产')
    await key.dispatchEvent('keydown', { key: 'Enter', code: 'Enter', isComposing: true })
    await page.waitForTimeout(300)
    expect(requests.filter((url) => url.pathname === '/admin/keys')).toHaveLength(0)
    expect(requests.filter((url) => url.pathname === '/admin/logs')).toHaveLength(1)
    await key.dispatchEvent('compositionend')
    await key.fill('production')
    await expect(page.getByRole('listbox').getByRole('option')).toHaveCount(2)
    const box = await page.locator('[popover]:popover-open').boundingBox()
    expect(box!.x).toBeGreaterThanOrEqual(8)
    expect(box!.x + box!.width).toBeLessThanOrEqual(width - 8)
    expect(box!.y).toBeGreaterThanOrEqual(4)
    expect(box!.y + box!.height).toBeLessThanOrEqual(796)
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    await page.screenshot({ path: `test-results/log-entity-search-${width}.png`, fullPage: true, animations: 'disabled' })
    await key.press('ArrowDown')
    await key.press('ArrowDown')
    await key.press('Enter')
    await expect(key).toHaveValue(longName)
    await expect(key).toHaveAccessibleDescription(`已选择 ${longName} · ID 20`)
  })
}
