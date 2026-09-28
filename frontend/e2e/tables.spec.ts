import { expect, test } from '@playwright/test'
import type { Locator, Page } from '@playwright/test'
import { fileURLToPath } from 'node:url'

function report(count = 40) {
  const data = Array.from({ length: count }, (_, i) => ({
    day: '2026-09-26', model: `model-${String(i + 1).padStart(2, '0')}`, requests: count - i,
    prompt_tokens: (count - i) * 1000, cached_tokens: (count - i) * 200, completion_tokens: (count - i) * 500,
    cache_write_tokens: null, reasoning_tokens: (count - i) * 50, amount_micro: (count - i) * 50000,
    discount_micro: 0, errors: 0,
  }))
  const sum = (field: 'prompt_tokens' | 'cached_tokens' | 'completion_tokens' | 'reasoning_tokens' | 'requests' | 'amount_micro') => data.reduce((n, row) => n + row[field], 0)
  return { data, days: 7, scope: 'key', live: null, total: {
    prompt_tokens: sum('prompt_tokens'), cached_tokens: sum('cached_tokens'), completion_tokens: sum('completion_tokens'), reasoning_tokens: sum('reasoning_tokens'),
    tokens: sum('prompt_tokens') + sum('completion_tokens'), requests: sum('requests'), amount_micro: sum('amount_micro'), discount_micro: 0,
    cache_hit_bp: 2000, cache_write_tokens: null, avg_rpm_micro: 1, avg_tpm_micro: 1,
  } }
}

async function prepare(page: Page) {
  const requests: URL[] = []
  await page.addInitScript(() => { localStorage.setItem('okapi.key', 'table-fixture'); localStorage.setItem('okapi.lang', 'zh-CN') })
  await page.route('**/*', (route) => {
    const request = route.request(), url = new URL(request.url())
    if (request.isNavigationRequest()) return route.fulfill({ path: fileURLToPath(new URL('../dist/index.html', import.meta.url)), contentType: 'text/html' })
    if (!/^\/(api|admin|auth)\//.test(url.pathname)) return route.continue()
    expect(request.method()).toBe('GET')
    requests.push(url)
    const offset = Number(url.searchParams.get('offset') ?? 0)
    const json = url.pathname === '/api/me' ? { user_id: 1, key_id: 1, role: 100, permissions: ['*'], balance_micro: 180000000, group: 'default' }
      : url.pathname === '/api/notice' ? { notice: null }
        : url.pathname === '/api/me/keys' ? { total: 1, data: [{ used_micro: 12300, requests: 20 }] }
          : url.pathname === '/api/me/stats/breakdown' ? report()
            : url.pathname === '/admin/groups' ? { total: 40, data: Array.from({ length: 20 }, (_, i) => ({ group_code: `group-${String(offset + i + 1).padStart(2, '0')}`, group_ratio: '1.5', description: 'Production group', user_count: 12, channel_count: 20, pool_code: 'default', is_default: false, self_select: false })) }
              : { data: [] }
    return route.fulfill({ json })
  })
  return requests
}

for (const view of ['models', 'tokens']) for (const width of [320, 390]) {
  test(`${view} 固定模型列 ${width}px：翻列前后保留重叠内容，不跳过被固定列遮住的数据`, async ({ page }) => {
    await prepare(page)
    await page.setViewportSize({ width, height: 844 })
    await page.goto(`/portal?view=${view}`)
    const table = page.getByRole('table', { name: view === 'models' ? '模型用量分布' : 'Token 构成', exact: true })
    await expect(table.locator('tbody tr')).toHaveCount(40)
    const viewport = table.locator('..')
    await viewport.evaluate((element) => element.scrollIntoView({ block: 'center' }))
    const forward = page.getByRole('group', { name: '表格横向浏览' }).getByRole('button', { name: '查看右侧列' })
    const before = await table.evaluate((element) => {
      const viewport = element.parentElement!
      const pinnedWidth = element.querySelector('th')!.getBoundingClientRect().width
      return { left: viewport.scrollLeft, visible: viewport.clientWidth - pinnedWidth }
    })
    const box = (await forward.boundingBox())!
    await page.mouse.click(box.x + box.width / 2, box.y + box.height / 2)
    await expect.poll(() => viewport.evaluate((element) => element.scrollLeft)).toBeGreaterThan(before.left)
    const moved = await viewport.evaluate((element) => element.scrollLeft) - before.left
    expect(moved).toBeLessThan(before.visible)
  })
}

test('横向浏览按钮：键盘聚焦和回退不改变表格原有滚动位置，回到最左侧可继续向右', async ({ page }) => {
  await prepare(page)
  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto('/portal?view=tokens')
  const table = page.getByRole('table', { name: 'Token 构成', exact: true })
  await expect(table.locator('tbody tr')).toHaveCount(40)
  const viewport = table.locator('..')
  await viewport.evaluate((element) => {
    element.scrollIntoView({ block: 'center' })
    element.scrollLeft = 180
    element.scrollTop = 240
  })
  const controls = page.getByRole('group', { name: '表格横向浏览' })
  const back = controls.getByRole('button', { name: '查看左侧列' })
  const forward = controls.getByRole('button', { name: '查看右侧列' })
  await expect(back).toBeEnabled()
  const position = await viewport.evaluate((element) => ({ left: element.scrollLeft, top: element.scrollTop, page: scrollY }))
  const expectPosition = async () => {
    const current = await viewport.evaluate((element) => ({ left: element.scrollLeft, top: element.scrollTop, page: scrollY }))
    for (const key of ['left', 'top', 'page'] as const) expect(Math.abs(current[key] - position[key])).toBeLessThanOrEqual(1)
  }
  await forward.focus()
  await expect(forward).toBeFocused()
  await expectPosition()
  await forward.press('Shift+Tab')
  await expect(back).toBeFocused()
  await expectPosition()
  await back.press('Enter')
  await expect.poll(() => viewport.evaluate((element) => element.scrollLeft)).toBeLessThan(position.left)
  expect(Math.abs(await viewport.evaluate((element) => element.scrollTop) - position.top)).toBeLessThanOrEqual(1)
  await viewport.evaluate((element) => { element.scrollLeft = 0 })
  await expect(back).toBeDisabled()
  await forward.focus()
  await forward.press('Enter')
  await expect.poll(() => viewport.evaluate((element) => element.scrollLeft)).toBeGreaterThan(0)
})

for (const view of ['models', 'tokens']) {
  test(`${view} 窄屏宽表：左右按钮与键盘可用，模型列和表头固定，变宽后收起提示`, async ({ page }) => {
    const requests = await prepare(page)
    await page.setViewportSize({ width: 390, height: 844 })
    await page.goto(`/portal?view=${view}`)
    const table = page.getByRole('table', { name: view === 'models' ? '模型用量分布' : 'Token 构成', exact: true })
    await expect(table.locator('tbody tr')).toHaveCount(40)
    const viewport = table.locator('..')
    await viewport.evaluate((element) => element.scrollIntoView({ block: 'center' }))
    const controls = page.getByRole('group', { name: '表格横向浏览' })
    const back = controls.getByRole('button', { name: '查看左侧列' })
    const forward = controls.getByRole('button', { name: '查看右侧列' })
    await expect(back).toBeDisabled()
    await expect(forward).toBeEnabled()
    const before = requests.length
    const pageTop = await page.evaluate(() => scrollY)
    // 保留真实指针点击覆盖；控件已独立于滚动区域，也可正常程序聚焦。
    const clickControl = async (control: Locator) => {
      const box = (await control.boundingBox())!
      await page.mouse.click(box.x + box.width / 2, box.y + box.height / 2)
    }
    await clickControl(forward)
    await expect.poll(() => viewport.evaluate((element) => element.scrollLeft)).toBeGreaterThan(0)
    await expect(back).toBeEnabled()
    const containerBox = (await viewport.boundingBox())!
    const controlsBox = (await controls.boundingBox())!
    const nameBox = (await table.locator('tbody tr').first().locator('td').first().boundingBox())!
    expect(Math.abs(controlsBox.x - containerBox.x - 1)).toBeLessThan(2)
    expect(Math.abs(nameBox.x - containerBox.x - 1)).toBeLessThan(2)
    expect(await page.evaluate(() => scrollY)).toBe(pageTop)
    expect(requests.length).toBe(before)
    await clickControl(back)
    await expect(back).toBeDisabled()
    await viewport.focus()
    await viewport.press('ArrowRight')
    await expect.poll(() => viewport.evaluate((element) => element.scrollLeft)).toBeGreaterThan(0)
    await viewport.evaluate((element) => { element.scrollLeft = element.scrollWidth; element.scrollTop = 280 })
    await expect(forward).toBeDisabled()
    const headerBox = (await table.getByRole('columnheader', { name: '模型', exact: true }).boundingBox())!
    const pinnedBox = (await controls.boundingBox())!
    expect(Math.abs(headerBox.y - pinnedBox.y - pinnedBox.height)).toBeLessThan(2)
    expect(Math.abs(pinnedBox.x - containerBox.x - 1)).toBeLessThan(2)
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    await page.screenshot({ path: `test-results/table-${view}-mobile.png`, fullPage: true, animations: 'disabled' })
    await page.setViewportSize({ width: 1800, height: 1000 })
    await expect(controls).toHaveCount(0)
    await viewport.evaluate((element) => { element.scrollTop = 280 })
    await expect(table.getByRole('columnheader', { name: '模型', exact: true })).toBeInViewport({ ratio: 1 })
  })
}

test('价格分组宽表：滚动列不影响翻页，换页复位纵向而保留横向位置', async ({ page }) => {
  const requests = await prepare(page)
  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto('/admin/groups')
  const table = page.getByRole('table'), viewport = table.locator('..')
  await expect(table.locator('tbody tr')).toHaveCount(20)
  await page.getByRole('group', { name: '表格横向浏览' }).getByRole('button', { name: '查看右侧列' }).click()
  await expect.poll(() => viewport.evaluate((element) => element.scrollLeft)).toBeGreaterThan(0)
  await viewport.evaluate((element) => { element.scrollTop = 240 })
  const left = await viewport.evaluate((element) => element.scrollLeft)
  await page.getByRole('navigation', { name: '分页' }).getByRole('button', { name: '下一页' }).click()
  await expect(table.locator('tbody tr').first()).toContainText('group-21')
  expect(await viewport.evaluate((element) => element.scrollTop)).toBe(0)
  expect(await viewport.evaluate((element) => element.scrollLeft)).toBe(left)
  expect(requests.filter((url) => url.pathname === '/admin/groups').map((url) => url.search)).toEqual(['?limit=20&offset=0', '?limit=20&offset=20'])
  await expect(page.getByRole('navigation', { name: '分页' })).toBeInViewport({ ratio: 1 })
  expect(await page.evaluate(() => document.documentElement.scrollHeight <= innerHeight + 1)).toBe(true)
})

for (const { width, language } of [
  { width: 1024, language: 'zh-CN' }, { width: 1920, language: 'zh-CN' },
  { width: 1440, language: 'en' }, { width: 390, language: 'zh-CN' },
]) {
  test(`价格分组列对齐 ${width}px ${language}：倍率、用户数、渠道数与限流沿表头右侧对齐`, async ({ page }) => {
    await prepare(page)
    await page.addInitScript((language) => {
      localStorage.setItem('okapi.lang', language)
      localStorage.setItem('okapi.theme', language === 'en' ? 'dark' : 'light')
    }, language)
    await page.emulateMedia({ reducedMotion: 'reduce' })
    await page.setViewportSize({ width, height: 1000 })
    const data = [
      { group_code: 'default', group_ratio: '1', user_count: 0, channel_count: 1, rpm_limit: null, rph_limit: null },
      { group_code: 'free', group_ratio: '1.2', user_count: 20, channel_count: 0, rpm_limit: 60, rph_limit: 1000 },
      { group_code: 'vip', group_ratio: '0.85', user_count: 1, channel_count: 12, rpm_limit: null, rph_limit: 10000 },
    ].map((row) => ({ ...row, description: row.group_code, pool_code: 'default', is_default: row.group_code === 'default', self_select: false }))
    await page.route('**/admin/groups?*', (route) => route.fulfill({ json: { total: data.length, data } }))
    await page.goto('/admin/groups')
    const table = page.getByRole('table', { name: language === 'en' ? 'Price groups' : '价格分组', exact: true })
    await expect(table.locator('tbody tr')).toHaveCount(3)
    const rightEdge = (cell: Locator) => cell.evaluate((element) => {
      const range = document.createRange()
      range.selectNodeContents(element)
      return range.getBoundingClientRect().right
    })
    for (const column of [1, 3, 5, 6]) {
      const header = table.locator('th').nth(column)
      await expect(header).toHaveCSS('text-align', 'right')
      for (const row of await table.locator('tbody tr').all()) {
        const cell = row.locator('td').nth(column)
        await expect(cell).toHaveCSS('text-align', 'right')
        // 检查实际文字边缘，而不仅是单元格边框；覆盖数字、空池徽标和无上限破折号。
        expect(Math.abs(await rightEdge(header) - await rightEdge(cell))).toBeLessThanOrEqual(1)
      }
    }
    const first = table.locator('tbody tr').first()
    await expect(first.locator('td').nth(6)).toHaveText('—')
    await expect(table.locator('tbody tr').nth(1).locator('td').nth(5)).toHaveText(language === 'en' ? 'Empty' : '空池')
    const actions = table.locator('th').last()
    for (const row of await table.locator('tbody tr').all()) {
      const lastButton = (await row.getByRole('button').last().boundingBox())!
      expect(Math.abs(await rightEdge(actions) - lastButton.x - lastButton.width)).toBeLessThanOrEqual(1)
    }
    if (width === 390) await table.locator('..').evaluate((element) => { element.scrollLeft = element.scrollWidth })
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    await page.screenshot({ path: `test-results/group-columns-${width}-${language}.png`, fullPage: true, animations: 'disabled' })
  })
}

test('小表不产生多余滚动控件或焦点停靠，无输入 Token 时不显示虚假缓存命中率', async ({ page }) => {
  await prepare(page)
  await page.setViewportSize({ width: 1800, height: 1000 })
  await page.route('**/api/me/stats/breakdown?*', (route) => {
    const data = report(1)
    Object.assign(data.data[0], { prompt_tokens: 0, cached_tokens: 0, amount_micro: 0 })
    Object.assign(data.total, { prompt_tokens: 0, cached_tokens: 0, amount_micro: 0, cache_hit_bp: 0 })
    return route.fulfill({ json: data })
  })
  await page.goto('/portal?view=models')
  const table = page.getByRole('table')
  await expect(table.locator('tbody tr')).toHaveCount(1)
  await expect(page.getByRole('group', { name: '表格横向浏览' })).toHaveCount(0)
  expect(await table.locator('..').getAttribute('tabindex')).toBeNull()
  await expect(table.locator('tbody tr td').nth(1)).toHaveText('—')
  await expect(table.locator('tbody tr td').last()).toHaveText('—')
  await page.getByRole('tab', { name: 'Token 构成' }).click()
  await expect(table.locator('tbody tr td').last()).toHaveText('—')
})

for (const { width, language, unpriced } of [
  { width: 1024, language: 'zh-CN', unpriced: 0 },
  { width: 1440, language: 'zh-CN', unpriced: 0 },
  { width: 1920, language: 'zh-CN', unpriced: 0 },
  { width: 1024, language: 'zh-CN', unpriced: 2 },
  { width: 1440, language: 'en', unpriced: 2 },
  { width: 390, language: 'zh-CN', unpriced: 2 },
]) {
  test(`模型定价排版 ${width}px ${language} 未定价${unpriced}：搜索与条数居中，发布区独立，数字列对齐`, async ({ page }) => {
    await prepare(page)
    await page.addInitScript(({ language }) => {
      localStorage.setItem('okapi.lang', language)
      localStorage.setItem('okapi.theme', language === 'en' ? 'dark' : 'light')
    }, { language })
    await page.emulateMedia({ reducedMotion: 'reduce' })
    await page.setViewportSize({ width, height: 1000 })
    const data = ['claude-sonnet-4-5', 'gemini-2.5-pro', 'gpt-4o', 'gpt-4o-audio-preview', 'gpt-4o-mini', 'gpt-6-astra'].map((model_name, i) => ({
      model_name, vendor: ['Anthropic', 'Google', 'OpenAI', 'OpenAI', 'OpenAI', 'OpenAI'][i],
      pricing_mode: i < unpriced ? null : 'ratio', model_ratio: i < unpriced ? null : '1.25', completion_ratio: i < unpriced ? null : '4',
      cache_ratio: '0.5', cache_write_ratio: '1.25', audio_ratio: '1', audio_completion_ratio: '1', image_ratio: '1',
    }))
    await page.route(/\/admin\/models(?:\?|$)/, (route) => route.fulfill({ json: { data, total: data.length, unpriced } }))
    await page.route('**/admin/channels', (route) => route.fulfill({ json: { data: [{ status: 1, models: data.map((row) => row.model_name) }] } }))
    await page.goto('/admin/pricing')
    const toolbar = page.locator('[data-slot="toolbar"]')
    const input = page.locator('#m-search')
    const count = toolbar.getByRole('status')
    const search = toolbar.getByRole('button', { name: language === 'en' ? 'Search' : '搜索', exact: true })
    const publish = toolbar.getByRole('button', { name: language === 'en' ? 'Publish pricing' : '发布定价', exact: true })
    const table = page.getByRole('table', { name: language === 'en' ? 'Model pricing' : '模型定价', exact: true })
    await expect(table.locator('tbody tr')).toHaveCount(6)
    await expect(count).toHaveText(language === 'en' ? '6 items' : '6 条')
    await expect(publish).toBeVisible()
    const inputBox = (await input.boundingBox())!, searchBox = (await search.boundingBox())!
    const countBox = (await count.boundingBox())!, publishBox = (await publish.boundingBox())!
    const centerY = (box: { y: number; height: number }) => box.y + box.height / 2
    expect(Math.abs(centerY(inputBox) - centerY(searchBox))).toBeLessThanOrEqual(1)
    expect(Math.abs(inputBox.height - publishBox.height)).toBeLessThanOrEqual(1)
    expect(inputBox.width).toBeLessThanOrEqual(400)
    expect(inputBox.width).toBeGreaterThanOrEqual(180)
    const controls = await toolbar.locator('input, button, [role="status"]').all()
    const boxes = await Promise.all(controls.map(async (control) => (await control.boundingBox())!))
    for (let i = 0; i < boxes.length; i++) for (let j = i + 1; j < boxes.length; j++) {
      const a = boxes[i], b = boxes[j]
      const overlapX = Math.min(a.x + a.width, b.x + b.width) - Math.max(a.x, b.x)
      const overlapY = Math.min(a.y + a.height, b.y + b.height) - Math.max(a.y, b.y)
      expect(overlapX > 1 && overlapY > 1, '工具栏控件不能相互遮挡').toBe(false)
    }
    if (width >= 1024) {
      expect(Math.abs(centerY(countBox) - centerY(searchBox))).toBeLessThanOrEqual(1)
      expect(Math.abs(centerY(publishBox) - centerY(searchBox))).toBeLessThanOrEqual(1)
      expect(countBox.x).toBeGreaterThanOrEqual(searchBox.x + searchBox.width)
      expect(publishBox.x).toBeGreaterThan(countBox.x + countBox.width)
      await expect(page.getByRole('navigation', { name: language === 'en' ? 'Pagination' : '分页' })).toBeInViewport({ ratio: 1 })
    }
    for (const index of [3, 4, 5, 6, 7, 8, 9]) {
      await expect(table.locator('th').nth(index)).toHaveCSS('text-align', 'right')
      await expect(table.locator('tbody tr').first().locator('td').nth(index)).toHaveCSS('text-align', 'right')
    }
    // 窄屏操作按钮保留 44px 触控区域，边框合并可多占 1px。
    for (const row of await table.locator('tbody tr').all()) expect(Math.abs((await row.boundingBox())!.height - 56)).toBeLessThanOrEqual(1)
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    await page.screenshot({ path: `test-results/pricing-layout-${width}-${language}-${unpriced}.png`, fullPage: true, animations: 'disabled' })
  })
}

// 尺寸契约：各列表共用字级、表头 / 行高、外框和底部分页，不依赖真实账号数据。
async function prepareListStyles(page: Page, count = 20) {
  await prepare(page)
  const rows = (factory: (id: number) => object) => ({ total: count, data: Array.from({ length: count }, (_, i) => factory(i + 1)) })
  const fixtures: Record<string, unknown> = {
    '/admin/roles': rows((id) => ({ id, role_code: `role-${id}`, display_name: `运营角色 ${id}`, permissions: ['channels.read'] })),
    '/admin/pools': rows((id) => ({ pool_code: `pool-${id}`, routing_strategy: 'priority_weighted', fallback_pool_code: null, description: '标准渠道池', channel_count: 2, group_count: 1, key_count: 0, fallback_ref_count: 0, builtin: false })),
    '/admin/users': rows((id) => ({ id, username: `user-${id}`, email: `user${id}@example.test`, role: 1, admin_role_id: null, status: 1, balance_micro: 12000000, price_multiplier: '1' })),
    '/api/me/keys': rows((id) => ({ id, name: `应用密钥 ${id}`, key_prefix: `sk-fixture-${id}`, status: 1, used_micro: 12300, amount_micro: 12300, requests: 20, rpm_limit: null, group_override: null, ip_allowlist: null, created_at: '2026-09-26T12:00:00Z' })),
    '/api/teams': rows((id) => ({ team_id: id, name: `团队 ${id}`, role: 'owner', member_count: 2, balance_micro: 12000000, monthly_spend_limit_micro: null })),
    '/api/me/ledger': { ...rows((id) => ({ event_id: id, event_type: 'recharge', delta_micro: 2000000, balance_after_micro: 12000000, pool: 0, source: 'payment', tags: [], request_id: null, created_at: '2026-09-26T12:00:00Z' })), next_before: null },
  }
  await page.route('**/*', (route) => {
    const request = route.request(), json = fixtures[new URL(request.url()).pathname]
    if (request.isNavigationRequest() || json === undefined) return route.fallback()
    expect(request.method()).toBe('GET')
    return route.fulfill({ json })
  })
}

for (const width of [1440, 390]) {
  test(`管理与门户列表 ${width}px：字级、表头、行高、分页位置一致`, async ({ page }) => {
    await prepareListStyles(page)
    await page.emulateMedia({ reducedMotion: 'reduce' })
    await page.setViewportSize({ width, height: 1000 })
    const footerBottoms: number[] = []
    for (const path of ['/admin/groups', '/admin/roles', '/admin/pools', '/admin/users', '/portal/keys', '/portal/teams', '/portal/ledger']) {
      await page.goto(path)
      const table = page.getByRole('table')
      await expect(table.locator('tbody tr')).toHaveCount(20)
      await expect(page.locator('[data-slot="page-header"] h1')).toHaveCSS('font-size', '20px')
      await expect(table.locator('th').first()).toHaveCSS('font-size', '12px')
      await expect(table.locator('td').first()).toHaveCSS('font-size', '13px')
      await expect(table.locator('td').first()).toHaveCSS('line-height', '20px')
      await expect(page.locator('[data-slot="table-frame"]')).toHaveCSS('border-radius', '16px')
      const header = (await table.locator('thead tr').boundingBox())!, row = (await table.locator('tbody tr').first().boundingBox())!
      expect(Math.abs(header.height - 40), path).toBeLessThanOrEqual(1)
      expect(Math.abs(row.height - 56), path).toBeLessThanOrEqual(1)
      const viewport = table.locator('..')
      expect(await viewport.evaluate((element) => element.scrollHeight > element.clientHeight), path).toBe(true)
      const footer = page.getByRole('navigation', { name: '分页' })
      if (path !== '/portal/ledger') {
        await expect(footer).toBeInViewport({ ratio: 1 })
        const box = (await footer.boundingBox())!
        footerBottoms.push(box.y + box.height)
      }
      expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), path).toBe(true)
      expect(await page.evaluate(() => document.documentElement.scrollHeight <= innerHeight + 1), path).toBe(true)
      await page.screenshot({ path: `test-results/style-${path.split('/').slice(1).join('-')}-${width}.png`, fullPage: true, animations: 'disabled' })
    }
    expect(Math.max(...footerBottoms) - Math.min(...footerBottoms)).toBeLessThanOrEqual(1)
  })
}

test('短列表、加载与空态使用同一剩余高度；长名称自动增高，小视口不裁切分页', async ({ page }) => {
  await prepareListStyles(page, 1)
  await page.setViewportSize({ width: 1280, height: 800 })
  await page.goto('/admin/roles')
  const frame = page.locator('[data-slot="table-frame"]')
  await expect(page.getByRole('table').locator('tbody tr')).toHaveCount(1)
  expect((await frame.boundingBox())!.height).toBeGreaterThan(400)
  await page.route('**/admin/roles?*', (route) => route.fulfill({ json: { total: 0, data: [] } }))
  await page.reload()
  const empty = page.locator('[data-slot="empty-state"]')
  await expect(empty).toBeVisible()
  const emptyHeight = (await empty.boundingBox())!.height
  expect(emptyHeight).toBeGreaterThan(400)

  let release!: () => void
  const gate = new Promise<void>((resolve) => { release = resolve })
  await page.route('**/admin/roles?*', async (route) => {
    await gate
    await route.fulfill({ json: { total: 1, data: [{ id: 1, role_code: 'long-role', display_name: '长名称自动换行，不裁切重要文字。'.repeat(30), permissions: [] }] } })
  })
  await page.reload()
  const skeleton = page.locator('[data-slot="table-skeleton"]')
  await expect(skeleton).toBeVisible()
  expect(Math.abs((await skeleton.boundingBox())!.height - emptyHeight)).toBeLessThanOrEqual(1)
  release()
  await expect(page.getByRole('table').locator('tbody tr')).toHaveCount(1)
  expect((await page.getByRole('table').locator('tbody tr').boundingBox())!.height).toBeGreaterThan(56)
  await page.setViewportSize({ width: 320, height: 568 })
  const footer = page.getByRole('navigation', { name: '分页' })
  await footer.scrollIntoViewIfNeeded()
  await expect(footer).toBeInViewport({ ratio: 1 })
  expect((await frame.boundingBox())!.height).toBeGreaterThanOrEqual(160)
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
})

test('英文与深色主题沿用表格尺寸，窄屏下可完整访问操作和分页', async ({ page }) => {
  await prepareListStyles(page, 2)
  await page.addInitScript(() => {
    localStorage.setItem('okapi.lang', 'en')
    localStorage.setItem('okapi.theme', 'dark')
  })
  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto('/admin/roles')
  await expect(page.locator('html')).toHaveClass(/dark/)
  const table = page.getByRole('table')
  await expect(table.locator('tbody tr')).toHaveCount(2)
  await expect(table.locator('td').first()).toHaveCSS('font-size', '13px')
  await expect(table.locator('th').first()).toHaveCSS('height', '40px')
  const viewport = table.locator('..')
  await viewport.evaluate((element) => { element.scrollLeft = element.scrollWidth })
  await expect(table.locator('tbody tr').first().getByRole('button').first()).toBeInViewport({ ratio: 1 })
  await page.locator('[data-slot="pagination"]').scrollIntoViewIfNeeded()
  await expect(page.locator('[data-slot="pagination"]')).toBeInViewport({ ratio: 1 })
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  await page.screenshot({ path: 'test-results/style-roles-dark-en.png', fullPage: true, animations: 'disabled' })
})
