import { expect, test } from '@playwright/test'
import type { Page } from '@playwright/test'
import { fileURLToPath } from 'node:url'

const groups = [
  { group_code: 'default', group_ratio: '1', description: '默认分组', is_default: true, self_select: false },
  { group_code: 'vip', group_ratio: '0.85', description: '专属优惠组', is_default: false, self_select: false },
  { group_code: 'free', group_ratio: '1.2', description: '开放组', is_default: false, self_select: true },
  ...Array.from({ length: 55 }, (_, i) => ({ group_code: `group-${i}`, group_ratio: '1', description: `分组 ${i}`, is_default: false, self_select: false })),
]

async function prepare(page: Page, options: { current?: string[]; catalogError?: boolean; overviewGate?: Promise<void>; dark?: boolean } = {}) {
  const state = { current: options.current ?? ['default'], catalogError: options.catalogError ?? false, saveError: false }
  const writes: { groups: { group_code: string; priority: number }[] }[] = []
  const catalogs: URL[] = []
  await page.addInitScript((dark) => {
    localStorage.setItem('okapi.key', 'user-groups-fixture')
    localStorage.setItem('okapi.lang', 'zh-CN')
    localStorage.setItem('okapi.theme', dark ? 'dark' : 'light')
  }, options.dark ?? false)
  await page.emulateMedia({ reducedMotion: 'reduce' })
  await page.route('**/*', async (route) => {
    const request = route.request(), url = new URL(request.url())
    if (request.isNavigationRequest()) return route.fulfill({ path: fileURLToPath(new URL('../dist/index.html', import.meta.url)), contentType: 'text/html' })
    if (!/^\/(api|admin|auth)\//.test(url.pathname)) return route.continue()
    const user = { id: 7, username: 'alice', role: 1, status: 1, balance_micro: 1000, price_multiplier: '1' }
    if (url.pathname === '/admin/groups') {
      catalogs.push(url)
      return state.catalogError ? route.fulfill({ status: 503, json: { error: { code: 'internal_error' } } }) : route.fulfill({ json: { data: groups, total: groups.length } })
    }
    if (url.pathname === '/admin/users/7/groups') {
      expect(request.method()).toBe('POST')
      const body = request.postDataJSON()
      writes.push(body)
      if (state.saveError) return route.fulfill({ status: 500, json: { error: { code: 'internal_error' } } })
      state.current = body.groups.map((group: { group_code: string }) => group.group_code)
      return route.fulfill({ json: { ok: true } })
    }
    if (url.pathname === '/admin/users/7/overview') {
      await options.overviewGate
      return route.fulfill({ json: { user, groups: state.current.map((code, i) => ({ code, priority: state.current.length - i })), keys: [] } })
    }
    expect(request.method()).toBe('GET')
    const responses: Record<string, unknown> = {
      '/api/me': { user_id: 1, key_id: 1, role: 100, permissions: ['*'], balance_micro: 1000, group: 'default' },
      '/api/notice': { notice: null }, '/admin/users': { data: [user], total: 1 },
      '/admin/users/7/usage': { days: 7, stats_available: false, daily: [], by_model: [], ledger: [] },
    }
    return route.fulfill({ json: responses[url.pathname] ?? { data: [] } })
  })
  await page.goto('/admin/users')
  await page.getByRole('row').filter({ hasText: 'alice' }).getByRole('button', { name: '管理', exact: true }).click()
  const drawer = page.getByRole('dialog')
  await drawer.getByRole('tab', { name: '分组', exact: true }).click()
  return { state, writes, catalogs, drawer, search: drawer.getByRole('combobox', { name: '添加系统分组' }), save: drawer.getByRole('button', { name: '保存', exact: true }) }
}

test('用户分组：只能选系统目录，关闭自选的组也可分配，搜索覆盖全量并按顺序提交', async ({ page }) => {
  const { drawer, search, save, writes, catalogs } = await prepare(page)
  await expect(search).toBeVisible()
  await expect(save).toBeDisabled()
  expect(catalogs[0].search).toBe('')
  await search.fill('arbitrary-group, fake-vip')
  await search.press('Enter')
  await save.focus()
  await expect(drawer.getByRole('list', { name: '已选分组（按优先级排序）' }).getByRole('listitem')).toHaveCount(1)
  await expect(save).toBeDisabled()
  expect(writes).toHaveLength(0)
  await search.fill('专属优惠')
  await expect(drawer.getByRole('option')).toHaveCount(1)
  await expect(drawer.getByRole('option')).toContainText('倍率 ×0.85 · 仅管理员分配')
  await search.press('ArrowDown')
  await search.press('Enter')
  await expect(search).toHaveValue('')
  await search.fill('vip')
  await expect(drawer.getByRole('option')).toHaveCount(0)
  await expect(drawer.getByText('没有可添加的匹配分组，请从系统已有分组中选择。')).toBeVisible()
  await search.fill('开放组')
  await expect(drawer.getByRole('option')).toContainText('开放自选')
  await drawer.getByRole('option').click()
  await search.fill('group-54')
  await drawer.getByRole('option', { name: /group-54/ }).click()
  await drawer.getByRole('button', { name: '提高 vip 的优先级', exact: true }).click()
  await drawer.getByRole('button', { name: '移除分组 free', exact: true }).click()
  await save.click()
  await expect.poll(() => writes.length).toBe(1)
  expect(writes[0]).toEqual({ groups: [
    { group_code: 'vip', priority: 3 }, { group_code: 'default', priority: 2 }, { group_code: 'group-54', priority: 1 },
  ] })
  await expect(save).toBeDisabled()
})

test('用户分组：目录加载失败不允许清空覆盖，可重试且保留原组', async ({ page }) => {
  const { state, drawer, search, save, writes } = await prepare(page, { catalogError: true })
  await expect(drawer.getByRole('alert')).toContainText('服务内部错误')
  await expect(save).toBeDisabled()
  expect(writes).toHaveLength(0)
  state.catalogError = false
  await drawer.getByRole('button', { name: '重试', exact: true }).click()
  await expect(search).toBeVisible()
  await expect(drawer.getByRole('button', { name: '移除分组 default', exact: true })).toBeVisible()
  await expect(save).toBeDisabled()
})

test('用户分组：迟到的用户详情正确回填，不能把加载中误认为没有分组', async ({ page }) => {
  let release!: () => void
  const overviewGate = new Promise<void>((resolve) => { release = resolve })
  const { drawer, search, save, writes } = await prepare(page, { current: ['vip', 'default'], overviewGate })
  await expect(save).toBeDisabled()
  await expect(search).toHaveCount(0)
  release()
  await expect(search).toBeVisible()
  const entries = drawer.getByRole('list', { name: '已选分组（按优先级排序）' }).getByRole('listitem')
  await expect(entries).toHaveCount(2)
  await expect(entries.first()).toContainText('vip')
  await expect(drawer.getByRole('button', { name: '提高 vip 的优先级', exact: true })).toBeDisabled()
  await expect(save).toBeDisabled()
  expect(writes).toHaveLength(0)
})

test('用户分组：已失效分组明确提示，不静默丢弃；移除全部后回到默认兜底', async ({ page }) => {
  const { drawer, save, writes } = await prepare(page, { current: ['default', 'removed-group'] })
  await expect(drawer.getByRole('alert')).toContainText('removed-group')
  await expect(save).toBeDisabled()
  await drawer.getByRole('button', { name: '移除分组 removed-group', exact: true }).click()
  await drawer.getByRole('button', { name: '移除分组 default', exact: true }).click()
  await expect(drawer.getByText('未分配专属分组，将使用系统默认分组。')).toBeVisible()
  await save.click()
  await expect.poll(() => writes.length).toBe(1)
  expect(writes[0]).toEqual({ groups: [] })
})

test('用户分组：保存失败保留选择与排序，重试不会混入搜索草稿', async ({ page }) => {
  const { state, drawer, search, save, writes } = await prepare(page)
  await search.fill('vip')
  await drawer.getByRole('option', { name: /vip/ }).click()
  await drawer.getByRole('button', { name: '提高 vip 的优先级', exact: true }).click()
  state.saveError = true
  await save.click()
  await expect(page.getByRole('alert')).toContainText('服务内部错误')
  await expect(drawer.getByRole('button', { name: '移除分组 vip', exact: true })).toBeVisible()
  await expect(save).toBeEnabled()
  state.saveError = false
  await search.fill('unlisted')
  await save.click()
  await expect.poll(() => writes.length).toBe(2)
  expect(writes[1]).toEqual(writes[0])
  expect(writes[1].groups.map((group) => group.group_code)).toEqual(['vip', 'default'])
})

for (const [width, dark] of [[1440, false], [390, true]] as const) test(`用户分组布局 ${width}：候选可读、排序按钮可达、抽屉不溢出`, async ({ page }) => {
  await page.setViewportSize({ width, height: 900 })
  const { drawer, search } = await prepare(page, { current: ['vip', 'default', 'free'], dark })
  await search.click()
  await expect(drawer.getByRole('option')).toHaveCount(40)
  const popover = drawer.locator('[popover]')
  const box = (await popover.boundingBox())!
  expect(box.x).toBeGreaterThanOrEqual(0)
  expect(box.x + box.width).toBeLessThanOrEqual(width)
  await search.press('Escape')
  await expect(drawer).toBeVisible()
  await expect(drawer.getByRole('list', { name: '已选分组（按优先级排序）' }).getByRole('listitem')).toHaveCount(3)
  expect(await drawer.evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true)
  await page.screenshot({ path: `test-results/user-groups-${width}.png`, animations: 'disabled' })
})
