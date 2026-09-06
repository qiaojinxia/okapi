import { expect, test } from '@playwright/test'
import type { Page } from '@playwright/test'
import { fileURLToPath } from 'node:url'

/// 订阅套餐（IMPLEMENTATION §11.28）门户交互：接口桩，不碰数据库。
const PLANS = [
  { plan_code: 'pro-monthly', display_name: 'Pro Monthly', quota_micro: 20_000_000, price_micro: 15_000_000, purchasable: true, period: 3, duration_days: 30, group_code: null, description: '$20 every month.', sort_order: 10 },
  { plan_code: 'starter-weekly', display_name: 'Starter Weekly', quota_micro: 3_000_000, price_micro: 2_500_000, purchasable: true, period: 2, duration_days: 28, group_code: 'vip', description: null, sort_order: 20 },
  { plan_code: 'vip-gift', display_name: 'VIP Gift', quota_micro: 50_000_000, price_micro: 0, purchasable: false, period: 3, duration_days: 90, group_code: null, description: null, sort_order: 30 },
]

const ACTIVE = {
  id: 7, plan_code: 'pro-monthly', display_name: 'Pro Monthly', period: 3, status: 1,
  quota_micro: 20_000_000, remaining_micro: 12_500_000, group_code: null, granted_group: false,
  starts_at: '2026-09-01T00:00:00Z', expires_at: '2026-10-01T00:00:00Z',
  window_start: '2026-09-01T00:00:00Z', window_end: '2026-10-01T00:00:00Z', pool_until_unix: 1_790_000_000, source: 'admin:2',
}

async function prepare(page: Page, subscription: typeof ACTIVE | null) {
  await page.addInitScript(() => {
    localStorage.setItem('okapi.key', 'subscription-ui-fixture')
    localStorage.setItem('okapi.lang', 'zh-CN')
  })
  const checkouts: unknown[] = []
  await page.route('**/*', async (route) => {
    const req = route.request()
    const url = new URL(req.url())
    if (req.isNavigationRequest()) {
      return route.fulfill({ path: fileURLToPath(new URL('../dist/index.html', import.meta.url)), contentType: 'text/html' })
    }
    if (!/^\/(api|admin|auth)\//.test(url.pathname)) return route.continue()
    if (url.pathname === '/api/me/subscriptions/checkout') {
      checkouts.push(req.postDataJSON())
      return route.fulfill({ json: { order_no: 'S-1', gateway: 'epay', pay_url: null } })
    }
    const json = url.pathname === '/api/me'
      ? { user_id: 2, key_id: 1, role: 1, group: 'default', permissions: [], balance_micro: 50_000_000, subscription_remaining_micro: subscription?.remaining_micro ?? 0, subscription_until_unix: subscription?.pool_until_unix ?? 0 }
      : url.pathname === '/api/plans' ? { data: PLANS }
      : url.pathname === '/api/me/subscription' ? { subscription, history: subscription ? [subscription] : [] }
      : url.pathname === '/api/notice' ? { notice: null }
      : { data: [] }
    return route.fulfill({ json })
  })
  return checkouts
}

test('无订阅：所有在售套餐可订阅，不售卖的只标兑换码；下单带 plan_code 与网关', async ({ page }) => {
  const checkouts = await prepare(page, null)
  await page.goto('/portal/plans')
  await expect(page.getByText('当前没有激活的订阅。')).toBeVisible()
  const buy = page.getByRole('button', { name: '订阅', exact: true })
  await expect(buy).toHaveCount(2)
  await expect(page.getByText('仅兑换码获得')).toHaveCount(1)
  await expect(page.getByText('含分组 vip')).toBeVisible()
  await page.getByRole('button', { name: 'Stripe', exact: true }).click()
  await buy.first().click()
  await expect.poll(() => checkouts.length).toBe(1)
  expect(checkouts[0]).toEqual({ plan_code: 'pro-monthly', gateway: 'stripe' })
  // 桩没回 pay_url → 页面用错误条说明，而不是静默
  await expect(page.getByText('网关未返回支付地址', { exact: false })).toBeVisible()
})

test('有订阅：当前套餐高亮可续期，其它在售套餐停用并说明原因，总览余额卡挂订阅剩余', async ({ page }) => {
  await prepare(page, ACTIVE)
  await page.goto('/portal/plans')
  await expect(page.getByText('当前套餐', { exact: true })).toBeVisible()
  await expect(page.getByText('本周期剩余').locator('..')).toContainText('US$12.50 / US$20.00')
  await expect(page.getByRole('progressbar')).toHaveAttribute('aria-valuenow', '63')
  await expect(page.getByRole('button', { name: '续期' })).toBeEnabled()
  const others = page.getByRole('button', { name: '订阅', exact: true })
  await expect(others).toHaveCount(1)
  await expect(others).toBeDisabled()
  await expect(page.getByText('当前套餐结束后可购买')).toHaveCount(1)

  await page.goto('/portal')
  await expect(page.getByRole('link', { name: '订阅剩余 US$12.50' })).toBeVisible()
})
