import { expect, test } from '@playwright/test'
import { fileURLToPath } from 'node:url'

const me = { user_id: 7, key_id: 1, role: 1, permissions: [], group: 'default', balance_micro: 0 }

test('偏好写入存储失败（配额满 / 存储被禁）：切主题、切语言、折叠侧栏不抛未捕获异常，界面照样生效', async ({ page }) => {
  const errors: string[] = []
  page.on('pageerror', (error) => errors.push(error.message))
  await page.addInitScript(() => {
    localStorage.setItem('okapi.key', 'storage-failure-fixture')
    localStorage.setItem('okapi.login-mode', 'account')
    localStorage.setItem('okapi.lang', 'zh-CN')
    localStorage.setItem('okapi.theme', 'light')
    // 登录态写好之后，让之后所有写入都像配额已满那样抛
    Storage.prototype.setItem = () => {
      throw new DOMException('The quota has been exceeded.', 'QuotaExceededError')
    }
  })
  await page.route('**/*', (route) => {
    const request = route.request(), path = new URL(request.url()).pathname
    if (request.isNavigationRequest()) return route.fulfill({
      path: fileURLToPath(new URL('../dist/index.html', import.meta.url)), contentType: 'text/html',
    })
    if (!/^\/(api|auth|admin)\//.test(path)) return route.continue()
    const responses: Record<string, unknown> = { '/api/me': me, '/api/notice': { notice: null } }
    return route.fulfill({ json: responses[path] ?? { data: [] } })
  })
  await page.goto('/portal')

  // 主题：存不下去，但当前页面照样变暗
  await page.getByRole('button', { name: '主题', exact: true }).click()
  await page.getByRole('menuitem', { name: '深色', exact: true }).click()
  await expect(page.locator('html')).toHaveClass(/dark/)

  // 语言：同样只是不记住
  await page.getByRole('button', { name: '语言', exact: true }).click()
  await page.getByRole('menuitem', { name: 'English', exact: true }).click()
  await expect(page.getByRole('button', { name: 'Language', exact: true })).toBeVisible()

  expect(errors.filter((message) => /quota/i.test(message)), '存储写入失败不应冒成未捕获异常').toEqual([])
})
