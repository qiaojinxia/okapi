import { expect, test } from '@playwright/test'
import type { Locator, Page } from '@playwright/test'
import { fileURLToPath } from 'node:url'

async function prepare(page: Page, language: string) {
  await page.addInitScript((language) => {
    localStorage.setItem('okapi.key', 'toolbar-fixture')
    localStorage.setItem('okapi.lang', language)
    localStorage.setItem('okapi.theme', language === 'en' ? 'dark' : 'light')
  }, language)
  await page.emulateMedia({ reducedMotion: 'reduce' })
  await page.route('**/*', (route) => {
    const request = route.request(), url = new URL(request.url())
    if (request.isNavigationRequest()) return route.fulfill({ path: fileURLToPath(new URL('../dist/index.html', import.meta.url)), contentType: 'text/html' })
    if (!/^\/(api|admin|auth)\//.test(url.pathname)) return route.continue()
    expect(request.method()).toBe('GET')
    const json = url.pathname === '/api/me' ? { user_id: 1, key_id: 1, role: 100, permissions: ['*'], balance_micro: 0, group: 'default' }
      : url.pathname === '/api/notice' ? { notice: null }
        : { data: [], total: 0, has_more: false, next_before: null }
    return route.fulfill({ json })
  })
}

async function expectNoOverlap(toolbar: Locator) {
  const frame = (await toolbar.boundingBox())!
  // 排除搜索框内部的清空按钮，和在自身滚动容器内浏览的分段按钮。
  const controls = await toolbar.locator('input, select, button[data-slot="button"]').all()
  const boxes = await Promise.all(controls.map(async (control) => (await control.boundingBox())!))
  for (const box of boxes) {
    expect(box.x).toBeGreaterThanOrEqual(frame.x)
    expect(box.x + box.width).toBeLessThanOrEqual(frame.x + frame.width)
    expect(box.y).toBeGreaterThanOrEqual(frame.y)
    expect(box.y + box.height).toBeLessThanOrEqual(frame.y + frame.height)
  }
  for (let i = 0; i < boxes.length; i++) for (let j = i + 1; j < boxes.length; j++) {
    const a = boxes[i], b = boxes[j]
    const x = Math.min(a.x + a.width, b.x + b.width) - Math.max(a.x, b.x)
    const y = Math.min(a.y + a.height, b.y + b.height) - Math.max(a.y, b.y)
    expect(x > 1 && y > 1, '筛选控件不能相互重叠').toBe(false)
  }
}

for (const { width, language } of [
  { width: 1024, language: 'zh-CN' }, { width: 1440, language: 'zh-CN' },
  { width: 1920, language: 'zh-CN' }, { width: 1440, language: 'en' },
  { width: 390, language: 'zh-CN' },
]) {
  test(`列表工具栏 ${width}px ${language}：搜索限宽、标签与按钮对齐，控件不重叠`, async ({ page }) => {
    await prepare(page, language)
    await page.setViewportSize({ width, height: 1000 })
    for (const name of ['channels', 'users', 'keys', 'audit', 'codes']) {
      await page.goto(`/admin/${name}`)
      const toolbar = page.locator('[data-slot="toolbar"]')
      await expect(toolbar).toBeVisible()
      await expect(page.locator('[data-slot="table-skeleton"]')).toHaveCount(0)
      await expectNoOverlap(toolbar)
      for (const group of await toolbar.locator('[data-slot="toolbar-search"]').all()) {
        const input = (await group.locator('input').boundingBox())!
        const button = (await group.locator('button[data-slot="button"]').boundingBox())!
        expect(input.width).toBeGreaterThanOrEqual(180)
        expect(input.width).toBeLessThanOrEqual(400)
        expect(Math.abs(input.y + input.height / 2 - button.y - button.height / 2)).toBeLessThanOrEqual(1)
      }
      if (width >= 1024) {
        for (const input of await toolbar.locator('input, select').all()) expect((await input.boundingBox())!.height).toBe(36)
        if (name === 'keys') {
          const first = (await page.locator('#kq').boundingBox())!, second = (await page.locator('#kuid').boundingBox())!
          const button = (await toolbar.getByRole('button', { name: language === 'en' ? 'Search' : '搜索', exact: true }).boundingBox())!
          expect(first.width).toBeLessThanOrEqual(320)
          expect(second.width).toBeLessThanOrEqual(320)
          expect(Math.abs(first.y - second.y)).toBeLessThanOrEqual(1)
          expect(Math.abs(first.y - button.y)).toBeLessThanOrEqual(1)
        }
        if (name === 'audit') {
          const boxes = await Promise.all(['#au-action', '#au-target', '#au-actor'].map(async (id) => (await page.locator(id).boundingBox())!))
          expect(Math.max(...boxes.map((b) => b.y)) - Math.min(...boxes.map((b) => b.y))).toBeLessThanOrEqual(1)
        }
      }
      expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
      await page.screenshot({ path: `test-results/toolbar-${name}-${width}-${language}.png`, fullPage: true, animations: 'disabled' })
    }
  })
}

test('令牌筛选错误不移动输入行；长搜索提示在自己的区域内换行', async ({ page }) => {
  await prepare(page, 'zh-CN')
  await page.setViewportSize({ width: 1440, height: 1000 })
  await page.goto('/admin/keys')
  const toolbar = page.locator('[data-slot="toolbar"]')
  const button = toolbar.getByRole('button', { name: '搜索', exact: true })
  const before = (await button.boundingBox())!
  await page.locator('#kuid').fill('-1')
  await button.click()
  await expect(page.locator('#kuid-error')).toBeVisible()
  expect((await button.boundingBox())!.y).toBe(before.y)
  expect((await page.locator('#kq').boundingBox())!.y).toBe((await page.locator('#kuid').boundingBox())!.y)
  await expectNoOverlap(toolbar)
  await page.goto(`/admin/users?q=${'very-long-user-name'.repeat(12)}`)
  await expect(page.locator('#u-search')).toBeVisible()
  await expectNoOverlap(toolbar)
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
})
