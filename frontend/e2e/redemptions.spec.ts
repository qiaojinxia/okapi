import { expect, test } from '@playwright/test'
import type { Page } from '@playwright/test'
import { fileURLToPath } from 'node:url'

async function prepare(page: Page, notice = false) {
  await page.addInitScript(() => {
    localStorage.setItem('okapi.key', 'redemption-ui-fixture')
    localStorage.setItem('okapi.lang', 'zh-CN')
  })
  const hits: string[] = []
  const rows = Array.from({ length: 45 }, (_, i) => ({
    id: 45 - i, batch_id: 'fixture-batch', amount_micro: 1000000, status: i < 30 ? 1 : 3,
    plan_code: null, bind_user_id: null, redeemed_by: null, redeemed_at: null,
    created_at: '2026-09-05T09:00:00Z',
  }))
  await page.route('**/*', async (route) => {
    const req = route.request()
    const url = new URL(req.url())
    if (req.isNavigationRequest()) {
      return route.fulfill({ path: fileURLToPath(new URL('../dist/index.html', import.meta.url)), contentType: 'text/html' })
    }
    if (!/^\/(api|admin|auth)\//.test(url.pathname)) return route.continue()
    expect(req.method()).toBe('GET')
    if (url.pathname === '/admin/redemptions') {
      hits.push(url.search)
      const status = url.searchParams.get('status')
      const filtered = rows.filter((r) => !status || r.status === Number(status))
      const start = Number(url.searchParams.get('offset'))
      const limit = Number(url.searchParams.get('limit'))
      return route.fulfill({ json: { total: filtered.length, data: filtered.slice(start, start + limit) } })
    }
    const json = url.pathname === '/api/me' ? {
      user_id: 1, key_id: 1, role: 100, group: 'default', permissions: ['*'], balance_micro: 0,
    } : url.pathname === '/api/notice' ? { notice: notice ? {
      title: '维护通知', body: '这是列表上方的公告，用于检查剩余可视空间。', level: 'info', updated_at: 'fixture-notice',
    } : null } : { data: [] }
    return route.fulfill({ json })
  })
  return hits
}

test('兑换码每页请求20条，翻页回首行，筛选复位，末页正确停用下一页', async ({ page }) => {
  const hits = await prepare(page)
  await page.goto('/admin/codes')
  const rows = page.locator('tbody tr')
  const pager = page.getByRole('navigation', { name: '分页' })
  await expect(rows).toHaveCount(20)
  await expect(pager).toContainText('1–20 / 共 45')
  expect(hits).toEqual(['?limit=20&offset=0'])
  const table = page.getByRole('table')
  await table.evaluate((el) => { el.parentElement!.scrollTop = 400 })
  await expect.poll(() => table.evaluate((el) => el.parentElement!.scrollTop)).toBeGreaterThan(0)
  await expect(page.getByRole('columnheader', { name: 'ID', exact: true })).toBeInViewport({ ratio: 1 })
  await pager.getByRole('button', { name: '下一页' }).click()
  await expect(rows.first().getByRole('cell').first()).toHaveText('25')
  expect(hits.at(-1)).toBe('?limit=20&offset=20')
  await expect.poll(() => table.evaluate((el) => el.parentElement!.scrollTop)).toBe(0)
  await pager.getByRole('button', { name: '下一页' }).click()
  await expect(rows).toHaveCount(5)
  await expect(pager).toContainText('41–45 / 共 45')
  await expect(pager.getByRole('button', { name: '下一页' })).toBeDisabled()
  await page.getByLabel('状态', { exact: true }).selectOption('1')
  await expect(rows).toHaveCount(20)
  await expect(pager).toContainText('1–20 / 共 30')
  expect(hits.at(-1)).toBe('?limit=20&offset=0&status=1')
  await page.getByLabel('状态', { exact: true }).selectOption('3')
  await expect(rows).toHaveCount(15)
  await expect(pager).toContainText('1–15 / 共 15')
  await expect(pager.getByRole('button', { name: '上一页' })).toBeDisabled()
  await expect(pager.getByRole('button', { name: '下一页' })).toBeDisabled()
})

for (const viewport of [{ width: 1280, height: 720 }, { width: 390, height: 844 }, { width: 320, height: 740 }]) {
  test(`兑换码${viewport.width}宽度：20行在表格内滚动，公告与分页可见，页面不溢出`, async ({ page }) => {
    await page.setViewportSize(viewport)
    await prepare(page, true)
    await page.goto('/admin/codes')
    await expect(page.locator('tbody tr')).toHaveCount(20)
    await expect(page.getByText('维护通知', { exact: true })).toBeVisible()
    const pager = page.getByRole('navigation', { name: '分页' })
    await expect(pager).toBeInViewport({ ratio: 1 })
    const table = page.getByRole('table')
    expect(await table.evaluate((el) => el.parentElement!.scrollHeight > el.parentElement!.clientHeight)).toBe(true)
    // 横、纵溢出留在表内，滚动到底时页头与分页仍完整可见。
    await table.evaluate((el) => { el.parentElement!.scrollTop = el.parentElement!.scrollHeight })
    await expect(page.getByRole('columnheader', { name: 'ID', exact: true })).toBeInViewport({ ratio: 1 })
    await expect(pager).toBeInViewport({ ratio: 1 })
    await expect.poll(() => page.evaluate(() => ({
      width: document.documentElement.scrollWidth <= innerWidth,
      height: document.documentElement.scrollHeight <= innerHeight + 1,
      scrollY,
    }))).toEqual({ width: true, height: true, scrollY: 0 })
    await page.screenshot({ path: `test-results/redemptions-${viewport.width}.png`, animations: 'disabled' })
    // 关闭公告释放高度，分页仍在当前视口中。
    await page.getByRole('main').getByRole('button', { name: '关闭', exact: true }).click()
    await expect(pager).toBeInViewport({ ratio: 1 })
  })
}
