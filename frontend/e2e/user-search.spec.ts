import { expect, test } from '@playwright/test'
import type { Page } from '@playwright/test'
import { fileURLToPath } from 'node:url'

async function prepare(page: Page, permissions = ['*']) {
  const requests: URL[] = []
  await page.addInitScript(() => { localStorage.setItem('okapi.key', 'user-search-fixture'); localStorage.setItem('okapi.lang', 'zh-CN') })
  await page.route('**/*', (route) => {
    const request = route.request(), url = new URL(request.url())
    if (request.isNavigationRequest()) return route.fulfill({ path: fileURLToPath(new URL('../dist/index.html', import.meta.url)), contentType: 'text/html' })
    if (!/^\/(api|admin|auth)\//.test(url.pathname)) return route.continue()
    expect(request.method()).toBe('GET')
    requests.push(url)
    const term = url.searchParams.get('q') ?? ''
    const people = [{ id: 7, username: 'alice', email: 'alice@example.test' }, { id: 42, username: 'bob-production', email: 'bob@example.test' }, { id: 18, username: '张三', email: 'zhang@example.test' }]
    const peopleFound = people.filter((user) => `${user.username} ${user.email}`.toLowerCase().includes(term.toLowerCase()))
    const uid = Number(url.searchParams.get('user_id') ?? 7)
    const offset = Number(url.searchParams.get('offset') ?? 0)
    const limit = Number(url.searchParams.get('limit') ?? 10)
    const total = term === 'no-results' ? 0 : 45
    const json = url.pathname === '/api/me' ? { user_id: 1, key_id: 1, role: 100, group: 'default', balance_micro: 50000000, permissions }
      : url.pathname === '/api/notice' ? { notice: null }
        : url.pathname === '/admin/users' ? { total: peopleFound.length, data: peopleFound }
          : url.pathname === '/admin/keys' ? { total, data: Array.from({ length: Math.max(0, Math.min(limit, total - offset)) }, (_, i) => ({
            id: offset + i + 1, user_id: uid, username: people.find((user) => user.id === uid)?.username ?? `user-${uid}`,
            name: `key-${offset + i + 1}`, key_prefix: `sk-prefix-${offset + i + 1}`, status: 1, used_micro: 1200000,
            rpm_limit: 60, expires_at: null, last_used_at: null, model_allowlist: null, ip_allowlist: null, group_override: null,
          })) } : { data: {} }
    return route.fulfill({ json })
  })
  return requests
}

test('用户候选：邮箱检索、键盘选择名称，提交准确 ID 并复位页码，刷新和后退保留筛选', async ({ page }) => {
  const requests = await prepare(page)
  await page.goto('/admin/keys?page=5')
  await expect(page.getByRole('table').locator('tbody tr')).toHaveCount(5)
  const user = page.getByRole('combobox', { name: '所属用户', exact: true })
  await user.fill('bob@example.test')
  await expect(page.getByRole('option', { name: /bob-production/ })).toBeVisible()
  expect(requests.filter((url) => url.pathname === '/admin/keys')).toHaveLength(1)
  await user.press('ArrowDown')
  await user.press('Enter')
  await expect(user).toHaveValue('bob-production')
  await expect(user).toHaveAccessibleDescription('已选择 bob-production · ID 42')
  await expect(page.getByRole('listbox')).toHaveCount(0)
  expect(requests.filter((url) => url.pathname === '/admin/keys')).toHaveLength(1)
  await page.getByRole('button', { name: '搜索', exact: true }).click()
  await expect(page).toHaveURL(/user_id=42/)
  await expect(page).not.toHaveURL(/page=/)
  await expect.poll(() => requests.filter((url) => url.pathname === '/admin/keys').at(-1)?.search).toBe('?limit=10&offset=0&user_id=42')
  await page.reload()
  await expect(user).toHaveValue('bob-production')
  await user.fill('alice')
  await user.press('Escape')
  await page.getByRole('button', { name: '搜索', exact: true }).click()
  await expect(user).toHaveAttribute('aria-invalid', 'true')
  await expect(page).toHaveURL(/user_id=42/)
  await page.getByRole('button', { name: '清空筛选', exact: true }).click()
  await expect(user).toHaveValue('')
  await expect(page).not.toHaveURL(/user_id=/)
  await page.goBack()
  await expect(user).toHaveValue('bob-production')
  for (const request of requests.filter((url) => url.pathname === '/admin/users')) {
    expect(request.searchParams.get('limit')).toBe('20')
    expect(request.searchParams.get('offset')).toBe('0')
  }
})

test('用户检索：中文输入不提前查询或提交，无效 ID 不扩大范围，直接输入 ID 仍可查询', async ({ page }) => {
  const requests = await prepare(page)
  await page.goto('/admin/keys?user_id=7')
  const user = page.getByRole('combobox', { name: '所属用户' })
  await expect(user).toHaveValue('alice')
  const keyQueries = requests.filter((url) => url.pathname === '/admin/keys').length
  await user.dispatchEvent('compositionstart')
  await user.fill('张')
  await user.dispatchEvent('keydown', { key: 'Enter', isComposing: true, bubbles: true })
  await page.waitForTimeout(350)
  expect(requests.filter((url) => url.pathname === '/admin/users')).toHaveLength(0)
  expect(requests.filter((url) => url.pathname === '/admin/keys')).toHaveLength(keyQueries)
  await user.dispatchEvent('compositionend')
  await page.getByRole('option', { name: /张三/ }).click()
  await expect(user).toHaveValue('张三')
  await page.getByRole('button', { name: '搜索', exact: true }).click()
  await expect(page).toHaveURL(/user_id=18/)
  for (const invalid of ['-1', '0', '1.2', '1e2', '9007199254740993']) {
    const before = requests.filter((url) => url.pathname === '/admin/keys').length
    await user.fill(invalid)
    await user.press('Enter')
    await expect(page.getByRole('alert')).toHaveText('请选择匹配的用户，或输入有效的正整数用户 ID。')
    await expect(page).toHaveURL(/user_id=18/)
    expect(requests.filter((url) => url.pathname === '/admin/keys')).toHaveLength(before)
  }
  await user.fill('')
  await user.pressSequentially('777')
  await expect(user).toHaveValue('777')
  await user.fill(' 77 ')
  await user.press('Enter')
  await expect(page).toHaveURL(/user_id=77/)
  await expect(page.getByRole('alert')).toHaveCount(0)
})

test('用户候选：旧响应不覆盖新搜索，空结果和失败可理解，失败仍可直接输入 ID', async ({ page }) => {
  await prepare(page)
  let release: () => void = () => undefined
  const pending = new Promise<void>((resolve) => { release = resolve })
  await page.route('**/admin/users?*', async (route) => {
    const q = new URL(route.request().url()).searchParams.get('q')
    if (q === 'bob') { await pending; return route.fulfill({ json: { total: 1, data: [{ id: 42, username: 'bob-old', email: null }] } }) }
    if (q === 'fail') return route.fulfill({ status: 500, json: { error: { code: 'internal_error' } } })
    return route.fallback()
  })
  await page.goto('/admin/keys')
  const user = page.getByRole('combobox', { name: '所属用户' })
  const oldQuery = page.waitForRequest((request) => new URL(request.url()).pathname === '/admin/users' && new URL(request.url()).searchParams.get('q') === 'bob')
  await user.fill('bob')
  await oldQuery
  await user.fill('zhang@example.test')
  await expect(page.getByRole('option', { name: /张三/ })).toBeVisible()
  release()
  await expect(page.getByRole('option', { name: /bob-old/ })).toHaveCount(0)
  await user.fill('no-match')
  await expect(page.getByText('没有匹配的用户，请换个名称、邮箱，或直接输入 ID。')).toBeVisible()
  await user.fill('fail')
  await expect(page.getByText('暂时无法加载用户候选，可直接输入用户 ID。')).toBeVisible()
  await user.fill('77')
  await user.press('Enter')
  await expect(page).toHaveURL(/user_id=77/)
  await page.getByRole('searchbox', { name: '搜索', exact: true }).fill('no-results')
  await page.getByRole('button', { name: '搜索', exact: true }).click()
  await expect(page.getByText('没有匹配的结果', { exact: true })).toBeVisible()
  await page.getByRole('button', { name: '清空筛选', exact: true }).last().click()
  await expect(page.getByRole('table').locator('tbody tr')).toHaveCount(10)
})

for (const width of [320, 390, 1280]) {
  test(`用户筛选 ${width}px：名称可读、候选不溢出；只读用户没有不可执行的写操作`, async ({ page }) => {
    await prepare(page, ['user.read', 'billing.read'])
    await page.setViewportSize({ width, height: 844 })
    await page.goto('/admin/keys?user_id=7')
    const user = page.getByRole('combobox', { name: '所属用户' })
    await expect(user).toHaveValue('alice')
    await expect(page.getByRole('button', { name: '停用', exact: true })).toHaveCount(0)
    await expect(page.getByRole('button', { name: '删除', exact: true })).toHaveCount(0)
    await expect(page.getByRole('link', { name: '查看这把令牌近 7 天的调用' }).first()).toBeVisible()
    await user.fill('bob')
    const option = page.getByRole('option', { name: /bob-production/ })
    await expect(option).toBeInViewport({ ratio: 1 })
    const box = (await option.boundingBox())!
    expect(box.x).toBeGreaterThanOrEqual(8)
    expect(box.x + box.width).toBeLessThanOrEqual(width - 8)
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    await page.screenshot({ path: `test-results/user-search-${width}.png`, fullPage: true, animations: 'disabled' })
  })
}

test('只有用户读取权限时，令牌列表不提供不可访问的日志入口或空操作列', async ({ page }) => {
  const requests = await prepare(page, ['user.read'])
  await page.goto('/admin/keys')
  await expect(page.getByRole('table').locator('tbody tr')).toHaveCount(10)
  await expect(page.getByRole('columnheader', { name: '操作', exact: true })).toHaveCount(0)
  await expect(page.getByRole('link', { name: '查看这把令牌近 7 天的调用' })).toHaveCount(0)
  expect(requests.filter((url) => url.pathname.startsWith('/admin/stats/'))).toHaveLength(0)
})

test('令牌翻页与筛选回到首行，加载下一页时旧行不能继续执行写操作', async ({ page }) => {
  await prepare(page)
  let release: () => void = () => undefined
  const pending = new Promise<void>((resolve) => { release = resolve })
  await page.route('**/admin/keys?*', async (route) => {
    if (new URL(route.request().url()).searchParams.get('offset') === '10') await pending
    return route.fallback()
  })
  await page.goto('/admin/keys')
  const table = page.getByRole('table'), viewport = table.locator('..')
  await expect(table.locator('tbody tr')).toHaveCount(10)
  await viewport.evaluate((element) => { element.scrollTop = 300 })
  await page.getByRole('navigation', { name: '分页' }).getByRole('button', { name: '下一页' }).click()
  await expect(table).toHaveAttribute('aria-busy', 'true')
  await expect(table.getByRole('button', { name: '停用', exact: true }).first()).toBeDisabled()
  await expect(table.getByRole('button', { name: '删除', exact: true }).first()).toBeDisabled()
  release()
  await expect(table.locator('tbody tr').first()).toContainText('key-11')
  await expect(table).toHaveAttribute('aria-busy', 'false')
  expect(await viewport.evaluate((element) => element.scrollTop)).toBe(0)
  await viewport.evaluate((element) => { element.scrollTop = 300 })
  await page.getByRole('searchbox', { name: '搜索', exact: true }).fill('key')
  await page.getByRole('button', { name: '搜索', exact: true }).click()
  await expect(table.locator('tbody tr').first()).toContainText('key-1')
  expect(await viewport.evaluate((element) => element.scrollTop)).toBe(0)
  await expect(page).not.toHaveURL(/page=/)
})
