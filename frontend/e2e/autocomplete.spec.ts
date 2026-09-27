import { expect, test } from '@playwright/test'
import type { Page } from '@playwright/test'
import { fileURLToPath } from 'node:url'

const models = [
  { model_name: 'gpt-alpha', display_name: '通用助手', vendor: 'OpenAI', pricing_mode: 'ratio' },
  { model_name: 'claude-beta', display_name: '写作助手', vendor: 'Anthropic', pricing_mode: 'ratio' },
  ...Array.from({ length: 55 }, (_, i) => ({ model_name: `model-${i}`, display_name: `测试模型 ${i}`, vendor: 'Demo', pricing_mode: 'ratio' })),
]

async function prepare(page: Page) {
  const requests: string[] = []
  await page.addInitScript(() => {
    localStorage.setItem('okapi.key', 'autocomplete-fixture')
    localStorage.setItem('okapi.lang', 'zh-CN')
  })
  await page.route('**/*', async (route) => {
    const request = route.request()
    const url = new URL(request.url())
    if (request.isNavigationRequest()) {
      return route.fulfill({ path: fileURLToPath(new URL('../dist/index.html', import.meta.url)), contentType: 'text/html' })
    }
    if (!/^\/(api|admin|auth)\//.test(url.pathname)) return route.continue()
    expect(request.method(), '此测试只操作表单草稿').toBe('GET')
    requests.push(url.pathname + url.search)
    const json = url.pathname === '/api/me' ? {
      user_id: 1, key_id: 1, role: 100, group: 'default', balance_micro: 0, permissions: ['*'],
    } : url.pathname === '/api/notice' ? { notice: null }
      : url.pathname === '/admin/models' && !url.search ? { data: models }
        : { data: [], total: 0, enabled: 0, unpriced: 0 }
    return route.fulfill({ json })
  })
  return requests
}

async function openUserGroups(page: Page) {
  await prepare(page)
  const user = { id: 7, username: 'alice', email: 'alice@ok.test', role: 1, status: 1, balance_micro: 0, admin_role_id: null, price_multiplier: '1' }
  await page.route('**/admin/users?*', (route) => route.fulfill({ json: { total: 1, data: [user] } }))
  await page.route('**/admin/users/7/overview', (route) => route.fulfill({ json: { user, groups: [{ code: 'default', priority: 1 }], keys: [] } }))
  await page.route('**/admin/users/7/usage?*', (route) => route.fulfill({ json: { days: 7, stats_available: false, daily: [], by_model: [], ledger: [] } }))
  const posts: { groups: { group_code: string; priority: number }[] }[] = []
  await page.route('**/admin/users/7/groups', (route) => {
    expect(route.request().method()).toBe('POST')
    posts.push(route.request().postDataJSON())
    return route.fulfill({ json: { ok: true } })
  })
  await page.goto('/admin/users')
  await page.getByRole('row').filter({ hasText: 'alice' }).getByRole('button', { name: '管理', exact: true }).click()
  const drawer = page.getByRole('dialog')
  await drawer.getByRole('tab', { name: '分组', exact: true }).click()
  return { input: drawer.getByPlaceholder('回车或逗号分隔，可粘贴多个'), save: drawer.getByRole('button', { name: '保存', exact: true }), posts }
}

for (const width of [320, 390, 1280]) {
  test(`标签输入${width}：粘贴多项后一次保存，失焦确认不移动按钮，提交顺序和去重正确`, async ({ page }) => {
    await page.setViewportSize({ width, height: 900 })
    const { input, save, posts } = await openUserGroups(page)
    const codes = Array.from({ length: 8 }, (_, i) => `research-team-${i}`)
    await input.fill(`default,${codes.join('，')},${codes[0]}`)
    await save.hover()
    const before = await save.boundingBox()
    await page.mouse.down()
    try {
      await expect(input).toHaveValue('')
      const after = await save.boundingBox()
      expect(Math.abs(after!.y - before!.y)).toBeLessThan(1)
    } finally { await page.mouse.up() }
    await expect.poll(() => posts.length).toBe(1)
    expect(posts[0]).toEqual({ groups: ['default', ...codes].map((group_code, i) => ({ group_code, priority: codes.length + 1 - i })) })
    expect(await page.getByRole('dialog').evaluate((node) => node.scrollWidth <= node.clientWidth)).toBe(true)
  })
}

test('标签输入：删除一项不确认其他草稿，重复项删除后不会失焦复活，焦点仍在输入框', async ({ page }) => {
  const { input, save, posts } = await openUserGroups(page)
  await input.fill('vip, research')
  await expect(page.getByRole('button', { name: '取消添加 vip', exact: true })).toBeVisible()
  await page.getByRole('button', { name: '移除 default', exact: true }).click()
  await expect(input).toBeFocused()
  await expect(input).toHaveValue('vip, research')
  await expect(page.getByRole('button', { name: '移除 research', exact: true })).toHaveCount(0)
  await page.getByRole('button', { name: '取消添加 vip', exact: true }).click()
  await expect(input).toBeFocused()
  await expect(input).toHaveValue('research')
  await input.press('Enter')
  await input.fill('research, next')
  await page.getByRole('button', { name: '移除 research', exact: true }).click()
  await expect(input).toHaveValue('next')
  await expect(input).toBeFocused()
  await save.click()
  await expect.poll(() => posts.length).toBe(1)
  expect(posts[0]).toEqual({ groups: [{ group_code: 'next', priority: 1 }] })
})

test('标签输入：多行粘贴保留分隔并替换选区，确认前可检查和撤销，输入光标位置正确', async ({ page }) => {
  const { input, save, posts } = await openUserGroups(page)
  await input.fill('default, old, tail')
  await input.evaluate((node: HTMLInputElement) => {
    node.setSelectionRange(9, 12)
    const clipboard = new DataTransfer()
    clipboard.setData('text/plain', 'vip\r\nresearch\n vip')
    node.dispatchEvent(new ClipboardEvent('paste', { bubbles: true, cancelable: true, clipboardData: clipboard }))
  })
  await expect(input).toHaveValue('default, vip, research,  vip, tail')
  await expect.poll(() => input.evaluate((node: HTMLInputElement) => node.selectionStart)).toBe(28)
  await expect(input).toHaveAccessibleDescription('已添加 1 项 · 待添加 3 项')
  await expect(page.getByRole('button', { name: '取消添加 vip', exact: true })).toHaveCount(1)
  await page.getByRole('button', { name: '取消添加 research', exact: true }).click()
  await save.click()
  await expect.poll(() => posts.length).toBe(1)
  expect(posts[0]).toEqual({ groups: [{ group_code: 'default', priority: 3 }, { group_code: 'vip', priority: 2 }, { group_code: 'tail', priority: 1 }] })
})

test('标签输入：标签只占一个 Tab 停靠点，方向键定位、Delete 删除、Esc 回输入而不关闭抽屉', async ({ page }) => {
  const { input, save } = await openUserGroups(page)
  const codes = Array.from({ length: 10 }, (_, i) => `team-${i}`)
  await input.fill(codes.join(','))
  await input.press('Enter')
  await input.press('Tab')
  await expect(page.getByRole('button', { name: '移除 default', exact: true })).toBeFocused()
  await page.keyboard.press('End')
  const last = page.getByRole('button', { name: '移除 team-9', exact: true })
  await expect(last).toBeFocused()
  await expect(last).toBeInViewport({ ratio: 1 })
  await page.keyboard.press('ArrowLeft')
  await expect(page.getByRole('button', { name: '移除 team-8', exact: true })).toBeFocused()
  await page.keyboard.press('Delete')
  await expect(input).toBeFocused()
  await expect(page.getByRole('button', { name: '移除 team-8', exact: true })).toHaveCount(0)
  await input.press('Tab')
  await page.keyboard.press('Home')
  await page.keyboard.press('Tab')
  await expect(save).toBeFocused()
  await page.keyboard.press('Shift+Tab')
  await page.keyboard.press('Escape')
  await expect(input).toBeFocused()
  await expect(page.getByRole('dialog')).toBeVisible()
  await input.press('Backspace')
  await expect(page.getByRole('button', { name: '移除 team-9', exact: true })).toHaveCount(0)
})

for (const width of [320, 1280]) {
  test(`标签输入${width}：长标签和批量草稿可滚动检查，触控与页面宽度保持可用`, async ({ page }) => {
    await page.setViewportSize({ width, height: 900 })
    const { input } = await openUserGroups(page)
    const long = 'research-team-with-a-long-name-that-must-wrap-without-widening-the-drawer'
    await input.fill(`${long},${Array.from({ length: 10 }, (_, i) => `group-${i}`).join(',')}`)
    const group = page.getByRole('group', { name: /^标签清单/ })
    expect(await group.evaluate((node) => node.scrollHeight > node.clientHeight)).toBe(true)
    await expect(group.getByRole('button', { name: `取消添加 ${long}`, exact: true })).toBeVisible()
    const box = await input.boundingBox()
    if (width === 320) expect(box!.height).toBeGreaterThanOrEqual(44)
    expect(await page.getByRole('dialog').evaluate((node) => node.scrollWidth <= node.clientWidth)).toBe(true)
    if (width === 320) await page.evaluate(() => document.documentElement.classList.add('dark'))
    await page.screenshot({ path: `test-results/tag-input-${width}.png`, animations: 'disabled' })
  })
}

test('模型联想：别名和厂商联合检索，点击直接搜索准确ID并复位页码，清空即恢复', async ({ page }) => {
  const requests = await prepare(page)
  await page.goto('/admin/pricing?page=3')
  const input = page.locator('#m-search')
  await input.fill('写作 anthropic')
  const option = page.getByRole('option')
  await expect(option).toHaveCount(1)
  await expect(option).toContainText('claude-beta')
  await expect(option).toContainText('写作助手 · Anthropic')
  await option.click()
  await expect(input).toHaveValue('claude-beta')
  await expect(page.getByRole('listbox')).toHaveCount(0)
  await expect.poll(() => requests.filter((path) => path.startsWith('/admin/models?')).at(-1)).toBe('/admin/models?limit=20&offset=0&q=claude-beta')
  await expect(page).toHaveURL(/q=claude-beta/)
  await page.getByRole('button', { name: '清空', exact: true }).click()
  await expect(input).toHaveValue('')
  await expect.poll(() => requests.filter((path) => path.startsWith('/admin/models?')).at(-1)).toBe('/admin/models?limit=20&offset=0')
  await expect(input).toBeFocused()
})

test('模型联想：方向键选择，中文确认不选择，Esc先收起候选再关闭抽屉', async ({ page }) => {
  await prepare(page)
  await page.goto('/admin/channels')
  await page.getByRole('button', { name: '路由诊断', exact: true }).click()
  const input = page.locator('#diag-model')
  await input.fill('gpt')
  await input.press('ArrowDown')
  await expect(input).toHaveAttribute('aria-activedescendant', /-0$/)
  await input.dispatchEvent('keydown', { key: 'Enter', code: 'Enter', isComposing: true, bubbles: true })
  await expect(input).toHaveValue('gpt')
  await input.press('Enter')
  await expect(input).toHaveValue('gpt-alpha')
  await expect(input).toBeFocused()
  await expect(page.getByRole('listbox')).toHaveCount(0)
  await input.fill('unregistered-model')
  await expect(page.getByText('暂无匹配项，可继续输入自定义内容')).toBeVisible()
  await input.press('Escape')
  await expect(page.getByRole('dialog')).toBeVisible()
  await expect(input).toHaveValue('unregistered-model')
  await expect(page.getByRole('listbox')).toHaveCount(0)
  await input.press('Escape')
  await expect(page.getByRole('dialog')).toHaveCount(0)
})

test('模型联想：精确ID排在首位，候选有数量上限且较后的模型仍能检索', async ({ page }) => {
  await prepare(page)
  await page.goto('/admin/pricing')
  const input = page.locator('#m-search')
  await input.click()
  await expect(page.getByRole('option')).toHaveCount(40)
  await expect(page.getByText('共 57 个匹配项，继续输入以缩小范围')).toBeVisible()
  await input.press('ArrowUp')
  await expect(page.getByRole('option').last()).toBeInViewport({ ratio: 1 })
  await expect(input).toBeFocused()
  await input.press('ArrowDown')
  await expect(page.getByRole('option').first()).toBeInViewport({ ratio: 1 })
  await input.fill('model-54')
  await expect(page.getByRole('option')).toHaveCount(1)
  await input.press('ArrowDown')
  await input.press('Enter')
  await expect(input).toHaveValue('model-54')
  await input.fill('model-1')
  await expect(page.getByRole('option').first()).toContainText('model-1测试模型 1')
  await input.press('ArrowDown')
  await input.press('Enter')
  await expect(input).toHaveValue('model-1')
})

for (const width of [1280, 390]) {
  test(`模型联想${width}：规则范围可点选，直接加入并去重，窄屏候选不被抽屉裁切`, async ({ page }) => {
    await page.setViewportSize({ width, height: 800 })
    await prepare(page)
    await page.goto('/admin/rules')
    await page.getByRole('main').getByRole('button', { name: /新建/ }).first().click()
    const input = page.locator('#s-models')
    await input.fill('通用')
    const option = page.getByRole('option', { name: /gpt-alpha/ })
    await expect(option).toBeInViewport({ ratio: 1 })
    await option.click()
    await expect(input).toHaveValue('')
    await expect(page.getByRole('button', { name: '移除 gpt-alpha', exact: true })).toHaveCount(1)
    await expect(input).toBeFocused()
    await input.fill('gpt-alpha')
    await expect(page.getByRole('option', { name: /gpt-alpha/ })).toHaveCount(0)
    await input.press('Enter')
    await expect(page.getByRole('button', { name: '移除 gpt-alpha', exact: true })).toHaveCount(1)
    await input.fill('custom-a，')
    await input.evaluate((node) => {
      const clipboard = new DataTransfer()
      clipboard.setData('text/plain', 'custom-b\ncustom-c')
      node.dispatchEvent(new ClipboardEvent('paste', { bubbles: true, cancelable: true, clipboardData: clipboard }))
    })
    await input.press('Tab')
    await expect(page.getByRole('button', { name: '移除 custom-a', exact: true })).toBeVisible()
    await expect(page.getByRole('button', { name: '移除 custom-b', exact: true })).toBeVisible()
    await expect(page.getByRole('button', { name: '移除 custom-c', exact: true })).toBeVisible()
    await input.fill('写作')
    await expect(page.getByRole('option', { name: /claude-beta/ })).toBeInViewport({ ratio: 1 })
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    await page.screenshot({ path: `test-results/autocomplete-${width}.png`, animations: 'disabled' })
  })
}

for (const width of [390, 768, 1280]) {
  test(`模型联想${width}：选择和取消匹配项保留其他已选模型，仅看已选可继续筛选`, async ({ page }) => {
    await page.setViewportSize({ width, height: 900 })
    await prepare(page)
    await page.goto('/admin/channels')
    await page.getByRole('button', { name: '新建渠道', exact: true }).first().click()
    const search = page.getByRole('searchbox', { name: '从已配定价的模型中选择' })
    await search.fill('通用 openai')
    await page.getByRole('button', { name: '选择匹配项', exact: true }).click()
    await expect(page.getByRole('checkbox', { name: 'gpt-alpha', exact: true })).toBeChecked()
    await search.fill('anthropic')
    await page.getByRole('button', { name: '选择匹配项', exact: true }).click()
    await page.getByRole('button', { name: '取消匹配项', exact: true }).click()
    await search.fill('')
    await expect(page.getByRole('checkbox', { name: 'gpt-alpha', exact: true })).toBeChecked()
    await expect(page.getByRole('checkbox', { name: 'claude-beta', exact: true })).not.toBeChecked()
    await page.locator('#d-models').fill('custom-provider/very-long-model-identifier-for-mobile-layout-check')
    await page.locator('#d-models').press('Enter')
    await page.getByRole('button', { name: '仅看已选', exact: true }).click()
    await expect(page.getByRole('button', { name: '仅看已选', exact: true })).toHaveAttribute('aria-pressed', 'true')
    await expect(page.getByRole('checkbox', { name: 'claude-beta', exact: true })).toHaveCount(0)
    await expect(page.getByRole('checkbox', { name: 'gpt-alpha', exact: true })).toBeChecked()
    await expect(page.getByRole('checkbox', { name: 'custom-provider/very-long-model-identifier-for-mobile-layout-check', exact: true })).toBeChecked()
    expect(await page.getByRole('dialog').evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true)
    await search.fill('anthropic')
    await expect(page.getByText('没有匹配的结果', { exact: true })).toBeVisible()
    await search.fill('')
    await page.getByRole('button', { name: '仅看已选', exact: true }).click()
    await expect(page.getByRole('checkbox', { name: 'gpt-alpha', exact: true })).toBeChecked()
    await expect(page.getByRole('checkbox', { name: 'claude-beta', exact: true })).not.toBeChecked()
    await search.scrollIntoViewIfNeeded()
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    await page.screenshot({ path: `test-results/model-picker-layout-${width}.png`, animations: 'disabled' })
  })
}
