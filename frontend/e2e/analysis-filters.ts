import { expect } from '@playwright/test'
import type { Page } from '@playwright/test'

/// 用量分析 / 服务质量的「高级筛选与比较」在右侧抽屉里编辑，页面上只留一个入口按钮和生效条件的标签。
/// 应用 / 重置之后抽屉收起；抽屉开着时页面上别的控件点不到——要继续改条件先重新打开。
export async function openAdvanced(page: Page) {
  const dialog = page.getByRole('dialog', { name: '高级筛选与比较', exact: true })
  if (!(await dialog.isVisible())) await appliedConditions(page).getByRole('button').first().click()
  await expect(dialog).toBeVisible()
  // 抽屉打开后 30ms 把焦点送进第一个字段；等它落定再操作，否则刚点开的日期浮层会被这次焦点移动收起
  await expect(dialog.locator(':focus')).toHaveCount(1)
}

/// 页面上「高级筛选与比较」入口那一块：按钮 + 已生效条件的标签。
export function appliedConditions(page: Page) {
  return page.locator('[data-slot="analysis-filter-summary"]')
}
