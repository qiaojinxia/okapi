import { expect, test } from '@playwright/test'
import type { Locator, Page } from '@playwright/test'
import { readdirSync, readFileSync } from 'node:fs'
import { join, relative } from 'node:path'
import { fileURLToPath } from 'node:url'

// 固定“现在”：月历里的“今天”、可选的最晚一天才稳定（时区一律按 UTC）。
const NOW = new Date('2026-09-15T12:00:00Z')
const LOGS = '/portal/logs?scope=user&start_date=2026-09-10&end_date=2026-09-13&timezone=UTC'

async function prepare(page: Page, language = 'zh-CN') {
  await page.addInitScript((language) => {
    localStorage.setItem('okapi.key', 'date-fixture')
    localStorage.setItem('okapi.lang', language)
  }, language)
  await page.clock.setFixedTime(NOW)
  await page.emulateMedia({ reducedMotion: 'reduce' })
  await page.route('**/*', (route) => {
    const request = route.request(), url = new URL(request.url())
    if (request.isNavigationRequest()) return route.fulfill({ path: fileURLToPath(new URL('../dist/index.html', import.meta.url)), contentType: 'text/html' })
    if (!/^\/(api|admin|auth)\//.test(url.pathname)) return route.continue()
    const json = url.pathname === '/api/me' ? { user_id: 1, key_id: 1, role: 100, permissions: ['*'], balance_micro: 0, group: 'default' }
      : url.pathname === '/api/notice' ? { notice: null }
        : url.pathname === '/admin/stats/trend' ? { days: 7, granularity: 'day', scope: {}, total: {}, previous: {}, data: [] }
          : { data: [], total: 0, has_more: false, next_before: null }
    return route.fulfill({ json })
  })
}

/// 入场动画走完再量位置：刚插进页面的那一帧里浮层还带着 scale / translate。
async function settled(panel: Locator) {
  await expect(panel).toBeVisible()
  await panel.evaluate((element) => Promise.all(element.getAnimations().map((animation) => animation.finished)))
}

async function openRange(page: Page, name = '2026-09-10 — 2026-09-13') {
  const trigger = page.getByRole('button', { name, exact: true })
  const panel = page.getByRole('dialog', { name: '自定义日期' })
  await trigger.click()
  await settled(panel)
  return { trigger, panel }
}

test('自定义日期在页面上只有一个按钮，点开是浮在页面上方的弹出框，不挤开版面', async ({ page }) => {
  await prepare(page)
  await page.goto('/portal/logs?scope=user&timezone=UTC')
  const filters = page.locator('[data-slot="log-filters"]')
  const trigger = filters.getByRole('button', { name: '自定义日期', exact: true })
  await expect(trigger).toHaveAttribute('aria-haspopup', 'dialog')
  await expect(trigger).toHaveAttribute('aria-expanded', 'false')
  await expect(page.locator('input[type=date]')).toHaveCount(0)
  const before = (await filters.boundingBox())!
  await trigger.click()
  const panel = page.getByRole('dialog', { name: '自定义日期' })
  await settled(panel)
  await expect(trigger).toHaveAttribute('aria-expanded', 'true')
  await expect(panel.locator('input[type=date]')).toHaveCount(2)
  await expect(panel.getByRole('grid')).toBeVisible()
  // 浮层挂在 body 上，不在筛选卡片里，卡片高度不变
  await expect(filters.getByRole('dialog')).toHaveCount(0)
  expect((await filters.boundingBox())!.height).toBe(before.height)
  const anchor = (await trigger.boundingBox())!, box = (await panel.boundingBox())!
  expect(box.y).toBeGreaterThanOrEqual(anchor.y + anchor.height)
  expect(box.y - (anchor.y + anchor.height)).toBeLessThan(16)
  await page.keyboard.press('Escape')
  await expect(panel).toHaveCount(0)
  await expect(trigger).toHaveAttribute('aria-expanded', 'false')
})

test('Esc 关闭并把焦点还给按钮；点外面、焦点移走也关闭；没应用的草稿不留', async ({ page }) => {
  await prepare(page)
  await page.goto(LOGS)
  const { trigger, panel } = await openRange(page)
  // 打开后焦点落在第一个输入框
  await expect(panel.getByLabel('开始日期')).toBeFocused()
  await panel.getByLabel('开始日期').fill('2026-09-01')
  await page.keyboard.press('Escape')
  await expect(panel).toHaveCount(0)
  await expect(trigger).toBeFocused()
  await expect(page).toHaveURL(/start_date=2026-09-10&end_date=2026-09-13/)
  // 再打开：草稿回到已应用的范围，而不是上次没应用的 09-01
  await trigger.click()
  await expect(panel.getByLabel('开始日期')).toHaveValue('2026-09-10')
  await page.locator('#main-content').getByRole('heading').first().click()
  await expect(panel).toHaveCount(0)
  await trigger.click()
  await expect(panel).toBeVisible()
  await page.getByRole('textbox', { name: '请求 ID', exact: true }).focus()
  await expect(panel).toHaveCount(0)
  // 取消按钮同样收起并还焦点
  await trigger.click()
  await panel.getByRole('button', { name: '取消', exact: true }).click()
  await expect(panel).toHaveCount(0)
  await expect(trigger).toBeFocused()
})

test('下方放不下就翻到按钮上方，整块都在窗口内', async ({ page }) => {
  await prepare(page)
  await page.goto(LOGS)
  const trigger = page.getByRole('button', { name: '2026-09-10 — 2026-09-13', exact: true })
  const anchor = (await trigger.boundingBox())!
  const height = Math.ceil(anchor.y + anchor.height) + 40
  await page.setViewportSize({ width: 1280, height })
  await trigger.click()
  const panel = page.getByRole('dialog', { name: '自定义日期' })
  await settled(panel)
  const above = (await trigger.boundingBox())!, box = (await panel.boundingBox())!
  expect(box.y).toBeGreaterThanOrEqual(0)
  expect(box.y + box.height).toBeLessThanOrEqual(above.y)
  // 放不下的部分在浮层自己里面滚，不是被窗口切掉
  expect(await panel.evaluate((element) => getComputedStyle(element).overflowY)).toBe('auto')
})

test('窗口临时缩得很小、触发器暂时落到窗口外，浮层不收起，恢复后仍在按钮下方', async ({ page }) => {
  await prepare(page)
  await page.goto(LOGS)
  const { trigger, panel } = await openRange(page)
  // 窗口缩放的瞬间（整页截图、最小化再还原都会出现）触发器会整个落到窗口外
  await page.setViewportSize({ width: 1280, height: 60 })
  await page.evaluate(() => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))))
  await expect(panel).toBeVisible()
  await page.setViewportSize({ width: 1280, height: 800 })
  await settled(panel)
  // 重新定位排在 resize 之后的下一帧：轮询到浮层回到按钮正下方，而不是立刻量
  const gap = async () => {
    const anchor = (await trigger.boundingBox())!, box = (await panel.boundingBox())!
    return box.y - (anchor.y + anchor.height)
  }
  await expect.poll(gap).toBeGreaterThanOrEqual(0)
  await expect.poll(gap).toBeLessThan(16)
})

test('月历点选：第一下落起点、第二下落终点，点在起点之前重新当起点；输入框与月历是同一份草稿', async ({ page }) => {
  await prepare(page)
  await page.goto(LOGS)
  const { panel } = await openRange(page)
  const start = panel.getByLabel('开始日期'), end = panel.getByLabel('结束日期')
  const apply = panel.getByRole('button', { name: '应用日期', exact: true })
  await expect(panel.getByRole('grid')).toHaveAttribute('data-month', '2026-09')
  await expect(panel.getByRole('gridcell', { selected: true })).toHaveCount(4)
  await panel.locator('[data-date="2026-09-10"]').click()
  await expect(start).toHaveValue('2026-09-10')
  await expect(end).toHaveValue('')
  await expect(apply).toBeDisabled()
  await panel.locator('[data-date="2026-09-03"]').click()
  await expect(start).toHaveValue('2026-09-03')
  await expect(end).toHaveValue('')
  await panel.locator('[data-date="2026-09-06"]').click()
  await expect(end).toHaveValue('2026-09-06')
  await expect(panel.getByRole('gridcell', { selected: true })).toHaveCount(4)
  await expect(apply).toBeEnabled()
  // 敲输入框，月历的色带跟着变
  await end.fill('2026-09-09')
  await expect(panel.getByRole('gridcell', { selected: true })).toHaveCount(7)
  // 起点敲到别的月份，月历翻到那个月
  await start.fill('2026-07-20')
  await expect(panel.getByRole('grid')).toHaveAttribute('data-month', '2026-07')
  await start.fill('2026-09-03')
  await apply.click()
  await expect(page.getByRole('dialog', { name: '自定义日期' })).toHaveCount(0)
  await expect(page).toHaveURL(/start_date=2026-09-03&end_date=2026-09-09/)
  await expect(page.getByRole('button', { name: '2026-09-03 — 2026-09-09', exact: true })).toBeVisible()
})

test('月历不能选未来的日子，也翻不到未来的月份', async ({ page }) => {
  await prepare(page)
  await page.goto(LOGS)
  const { panel } = await openRange(page)
  await expect(panel.locator('[data-date="2026-09-15"]')).toHaveAttribute('aria-current', 'date')
  await expect(panel.locator('[data-date="2026-09-16"]')).toHaveAttribute('aria-disabled', 'true')
  await panel.locator('[data-date="2026-09-16"]').click({ force: true })
  await expect(panel.getByLabel('结束日期')).toHaveValue('2026-09-13')
  await expect(panel.getByRole('button', { name: '下个月', exact: true })).toBeDisabled()
  await expect(panel.getByRole('button', { name: '下一年', exact: true })).toBeDisabled()
  await expect(panel.getByRole('button', { name: '上个月', exact: true })).toBeEnabled()
  await expect(panel.getByLabel('结束日期')).toHaveAttribute('max', '2026-09-15')
  // 输入框里手敲未来的日子：不能应用
  await panel.getByLabel('结束日期').fill('2026-09-20')
  await expect(panel.getByRole('button', { name: '应用日期', exact: true })).toBeDisabled()
})

test('月历键盘：方向键 / Home / End / PageUp 在日历里移动，Enter 与空格依次选起止', async ({ page }) => {
  await prepare(page)
  await page.goto(LOGS)
  const { panel } = await openRange(page)
  const day = (value: string) => panel.locator(`[data-date="${value}"]`)
  // 只有一个日子在 Tab 序里
  await expect(panel.locator('[data-date][tabindex="0"]')).toHaveCount(1)
  await day('2026-09-10').focus()
  await page.keyboard.press('ArrowRight')
  await expect(day('2026-09-11')).toBeFocused()
  await page.keyboard.press('ArrowDown')
  await expect(day('2026-09-15'), '下一周越过今天，停在今天').toBeFocused()
  await page.keyboard.press('Home')
  await expect(day('2026-09-14'), '中文周一起').toBeFocused()
  await page.keyboard.press('PageUp')
  await expect(panel.getByRole('grid')).toHaveAttribute('data-month', '2026-08')
  await expect(day('2026-08-14')).toBeFocused()
  await page.keyboard.press('Shift+PageUp')
  await expect(panel.getByRole('grid')).toHaveAttribute('data-month', '2025-08')
  await expect(day('2025-08-14')).toBeFocused()
  await page.keyboard.press('Enter')
  await expect(panel.getByLabel('开始日期')).toHaveValue('2025-08-14')
  await expect(panel.getByLabel('结束日期')).toHaveValue('')
  await page.keyboard.press('ArrowRight')
  await page.keyboard.press(' ')
  await expect(panel.getByLabel('结束日期')).toHaveValue('2025-08-15')
  await expect(panel.getByRole('button', { name: '应用日期', exact: true })).toBeEnabled()
})

for (const [language, first, trigger] of [['zh-CN', '星期一', '自定义日期'], ['en', 'Sunday', 'Custom dates']] as const) {
  test(`周起点跟语言走 ${language}：${first} 排第一`, async ({ page }) => {
    await prepare(page, language)
    await page.goto('/portal/logs?scope=user&timezone=UTC')
    await page.getByRole('button', { name: trigger, exact: true }).click()
    const panel = page.getByRole('dialog', { name: trigger })
    await settled(panel)
    await expect(panel.locator('th').first()).toHaveAttribute('abbr', first)
    await expect(panel.locator('th')).toHaveCount(7)
  })
}

test('管理端日志的自定义时间：点开才见输入，Esc 退回原来的区间，应用才生效并收起', async ({ page }) => {
  await prepare(page)
  await page.goto('/admin/logs')
  const range = page.getByRole('region', { name: '时间窗' })
  const trigger = range.getByRole('button', { name: '自定义时间', exact: true })
  const panel = page.getByRole('dialog', { name: '自定义时间' })
  await expect(page.locator('#logs-from')).toHaveCount(0)
  await trigger.click()
  await settled(panel)
  await page.locator('#logs-from').fill('2026-09-10T08:00')
  await page.locator('#logs-to').fill('2026-09-11T08:00')
  await page.keyboard.press('Escape')
  await expect(panel).toHaveCount(0)
  await expect(range.getByRole('button', { name: '自定义时间', exact: true })).toBeFocused()
  await expect(page).not.toHaveURL(/from=/)
  await trigger.click()
  await expect(page.locator('#logs-from')).toHaveValue('')
  await page.locator('#logs-from').fill('2026-09-10T08:00')
  await page.locator('#logs-to').fill('2026-09-11T08:00')
  await panel.getByRole('button', { name: '应用区间', exact: true }).click()
  await expect(panel).toHaveCount(0)
  await expect(page).toHaveURL(/from=/)
  await expect(range.locator('button[aria-haspopup=dialog]')).toContainText('2026-09-10 08:00')
})

test('分析页的起止日期是弹出框字段：确定只写进草稿，不会把外层高级筛选表单一起提交', async ({ page }) => {
  await prepare(page)
  await page.goto('/admin/quality')
  await page.getByText('高级筛选与比较', { exact: true }).click()
  // 草稿里先改一项别的：外层表单若被误提交，这一项会被连带应用到地址里
  await page.getByLabel('时间粒度').selectOption('hour')
  const field = page.getByRole('button', { name: /^起止日期/ })
  await expect(field).toContainText('选择起止日期')
  await expect(page.locator('input[type=date]')).toHaveCount(0)
  await field.click()
  const panel = page.getByRole('dialog', { name: '起止日期' })
  await settled(panel)
  await panel.locator('[data-date="2026-09-03"]').click()
  await panel.locator('[data-date="2026-09-09"]').click()
  await panel.getByRole('button', { name: '确定', exact: true }).click()
  await expect(panel).toHaveCount(0)
  await expect(field).toContainText('2026-09-03 — 2026-09-09')
  await expect(field).toBeFocused()
  // 浮层里表单的 submit 没有冒泡到外层表单：粒度没被连带应用，地址没变，也没有报错
  await page.waitForTimeout(300)
  expect(page.url()).not.toMatch(/granularity|start_date/)
  await expect(page.getByRole('alert')).toHaveCount(0)
  await page.getByRole('button', { name: '应用分析条件' }).click()
  await expect(page).toHaveURL(/granularity=hour/)
  await expect(page).toHaveURL(/start_date=2026-09-03&end_date=2026-09-09/)
  // 重新打开有「清空」：清掉后两端都空，只写进草稿，仍要应用才生效
  await field.click()
  await panel.getByRole('button', { name: '清空', exact: true }).click()
  await expect(panel).toHaveCount(0)
  await expect(field).toContainText('选择起止日期')
  await expect(page).toHaveURL(/start_date=2026-09-03/)
  await page.getByRole('button', { name: '应用分析条件' }).click()
  await expect(page).not.toHaveURL(/start_date/)
})

test('筛选里的日期输入只出现在弹出框的内容里，页面上不再直接铺日期输入', () => {
  const root = fileURLToPath(new URL('../src/', import.meta.url))
  const files: string[] = []
  const walk = (dir: string) => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const path = join(dir, entry.name)
      if (entry.isDirectory()) walk(path)
      else if (entry.name.endsWith('.tsx')) files.push(path)
    }
  }
  walk(root)
  const hits = files.filter((file) => /type=["'](date|datetime-local|time|month|week)["']/.test(readFileSync(file, 'utf8'))).map((file) => relative(root, file).replaceAll('\\', '/')).sort()
  // 新加日期输入请放进 Popover（见 components/ui/date-range.tsx），再把文件登记到这里
  expect(hits).toEqual([
    'components/ui/date-range.tsx',
    'features/admin-logs/AdminLogsPage.tsx',
    // 表单里的单个到期时间 / 日期（点输入框本身弹原生选择器），不是筛选里的“自定义日期”
    'features/codes/GenerateDrawer.tsx',
    'features/portal-keys/KeyLimitsFields.tsx',
    'features/users/BalanceSection.tsx',
  ])
})
