import { expect, test } from '@playwright/test'
import type { Page } from '@playwright/test'
import { fileURLToPath } from 'node:url'

// 「Powered by Okapi」与原项目链接：README「许可证」依 AGPL-3.0 第 7 条要求修改版保留。
// 登录页、门户 / 后台外壳、公开价格页都要有，改版时别顺手删掉。

const REPO = 'https://github.com/qiaojinxia/okapi'

async function prepare(page: Page, { signedIn }: { signedIn: boolean }) {
  await page.addInitScript((signedIn) => {
    localStorage.setItem('okapi.lang', 'zh-CN')
    localStorage.setItem('okapi.guide.7', 'dismissed')
    if (signedIn) {
      localStorage.setItem('okapi.key', 'attribution-fixture')
      localStorage.setItem('okapi.login-mode', 'account')
    }
  }, signedIn)
  await page.route('**/*', (route) => {
    const request = route.request(), path = new URL(request.url()).pathname
    if (request.isNavigationRequest()) return route.fulfill({ path: fileURLToPath(new URL('../dist/index.html', import.meta.url)), contentType: 'text/html' })
    if (!/^\/(api|admin|auth)\//.test(path)) return route.continue()
    const responses: Record<string, unknown> = {
      '/api/me': { user_id: 7, key_id: 1, has_web_session: true, role: 1, permissions: [], balance_micro: 0, group: 'default' },
      '/api/setup/status': { needs_setup: false },
      '/api/registration': {
        mode: 'open', email_verification: false, allowed_domains: [],
        new_user_credit_micro: 0, invitee_credit_micro: 0,
      },
      '/auth/oauth-providers': { providers: [] },
      '/api/notice': { notice: null },
      '/api/pricing': { models: [], groups: [] },
      '/api/me/keys': { total: 0, data: [] },
    }
    return route.fulfill({ json: responses[path] ?? { data: [] } })
  })
}

async function expectAttribution(page: Page) {
  const link = page.locator(`a[href="${REPO}"]`)
  await expect(link).toHaveCount(1)
  await expect(link).toBeVisible()
  await expect(link).toHaveText('Okapi')
  await expect(link.locator('..')).toHaveText('Powered by Okapi')
  await expect(link).toHaveAttribute('target', '_blank')
  await expect(link).toHaveAttribute('rel', /noopener/)
}

test('登录页底部带「Powered by Okapi」与原项目链接，手机宽度同样可见', async ({ page }) => {
  await prepare(page, { signedIn: false })
  await page.goto('/')
  await expect(page.getByRole('button', { name: '登录', exact: true })).toBeVisible()
  await expectAttribution(page)
  await page.setViewportSize({ width: 390, height: 844 })
  await expectAttribution(page)
})

test('门户外壳侧栏底部带署名（后台用同一个外壳）', async ({ page }) => {
  await prepare(page, { signedIn: true })
  await page.goto('/portal/keys')
  await expect(page.locator('#main-content')).toBeVisible()
  await expectAttribution(page)
})

test('公开价格页页脚带署名', async ({ page }) => {
  await prepare(page, { signedIn: false })
  await page.goto('/pricing')
  await expectAttribution(page)
})
