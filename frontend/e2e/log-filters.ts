import type { Page } from '@playwright/test'

/// 管理端日志的「更多筛选」是浮层，不在页面里展开：要看 / 改里面的字段先点开（已经开着就不动）。
/// 点浮层外面（搜索、清空筛选）或重新加载页面都会把它收起，之后再读字段要重新点开。
export async function openMoreFilters(page: Page) {
  const field = page.locator('#lf-user_id')
  if (!(await field.isVisible())) await page.getByRole('button', { name: '更多筛选', exact: true }).click()
  await field.waitFor()
}
