import type { Locator } from '@playwright/test'

/// 月历弹出框里翻到某个月、再点某一天（`day` = 'YYYY-MM-DD'）。
/// 当前显示的月份在 `<table data-month>` 上；相差一年以上先用「上 / 下一年」大步走。
export async function pickDay(dialog: Locator, day: string): Promise<void> {
  const index = (month: string) => Number(month.slice(0, 4)) * 12 + Number(month.slice(5, 7))
  const target = index(day)
  for (let step = 0; step < 60; step++) {
    const shown = await dialog.getByRole('grid').getAttribute('data-month')
    const gap = target - index(shown!)
    if (gap === 0) break
    const name = Math.abs(gap) >= 12 ? (gap > 0 ? /^(下一年|Next year)$/ : /^(上一年|Previous year)$/) : gap > 0 ? /^(下个月|Next month)$/ : /^(上个月|Previous month)$/
    await dialog.getByRole('button', { name }).click()
  }
  await dialog.locator(`[data-date="${day}"]`).click()
}
