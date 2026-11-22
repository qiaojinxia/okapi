import { expect, test } from '@playwright/test'
import type { Page } from '@playwright/test'
import { fileURLToPath } from 'node:url'

const fixtureKey = 'fixture-timezone-login-secret'

async function prepare(page: Page, value: string | null = null) {
  const state = { value: value as string | null, posts: [] as unknown[], saveStatus: 200 }
  await page.addInitScript((key) => {
    localStorage.setItem('okapi.key', key)
    localStorage.setItem('okapi.lang', 'zh-CN')
  }, fixtureKey)
  await page.route('**/*', async (route) => {
    const request = route.request(), path = new URL(request.url()).pathname
    if (request.isNavigationRequest()) return route.fulfill({ path: fileURLToPath(new URL('../dist/index.html', import.meta.url)), contentType: 'text/html' })
    if (!/^\/(api|admin|auth)\//.test(path)) return route.continue()
    if (path === '/admin/settings' && request.method() === 'POST') {
      const body = request.postDataJSON()
      state.posts.push(body)
      if (state.saveStatus !== 200) return route.fulfill({ status: state.saveStatus, json: { error: { code: 'bad_request', param: 'reporting_timezone_whole_hours' } } })
      state.value = body.value
      return route.fulfill({ json: { ok: true, requires_publish: false } })
    }
    expect(request.method()).toBe('GET')
    if (path === '/admin/settings/reporting_timezone') return route.fulfill({
      json: { key: 'reporting_timezone', value: state.value, host_timezone: 'Etc/UTC', effective_timezone: state.value ?? 'Etc/UTC' },
    })
    const json = path === '/api/me' ? { user_id: 1, key_id: 1, role: 100, permissions: ['*'], group: 'default', balance_micro: 0 }
      : path.startsWith('/admin/settings/') ? { value: null } : { data: [] }
    return route.fulfill({ json })
  })
  return state
}

test.use({ timezoneId: 'Asia/Shanghai' })

test('统计时区：缺省跟随服务器，选时区保存写入 IANA 名，换回跟随服务器写 null', async ({ page }) => {
  const state = await prepare(page)
  await page.goto('/admin/settings?tab=timezone')
  const select = page.getByRole('combobox', { name: '时区', exact: true })
  await expect(select).toHaveValue('')
  await expect(select.locator('option').first()).toHaveText('跟随服务器（Etc/UTC UTC+00:00）')
  await expect(page.getByTestId('reporting-timezone-effective')).toHaveText('当前生效：Etc/UTC UTC+00:00')
  const save = page.getByRole('button', { name: '保存', exact: true })
  await expect(save).toBeDisabled()

  // 浏览器时区快捷选择 → 保存
  await page.getByRole('button', { name: '改用本浏览器的时区（Asia/Shanghai）' }).click()
  await expect(select).toHaveValue('Asia/Shanghai')
  await save.click()
  await expect.poll(() => state.posts).toEqual([{ key: 'reporting_timezone', value: 'Asia/Shanghai' }])
  await expect(page.getByTestId('reporting-timezone-effective')).toHaveText('当前生效：Asia/Shanghai UTC+08:00')
  await expect(save).toBeDisabled()

  // 换回跟随服务器：写 null，不写空串
  await select.selectOption('')
  await save.click()
  await expect.poll(() => state.posts.length).toBe(2)
  expect(state.posts[1]).toEqual({ key: 'reporting_timezone', value: null })
})

test('统计时区：与 UTC 差半小时的时区置灰不可选，已保存的值能回显', async ({ page }) => {
  await prepare(page, 'America/Los_Angeles')
  await page.goto('/admin/settings?tab=timezone')
  const select = page.getByRole('combobox', { name: '时区', exact: true })
  await expect(select).toHaveValue('America/Los_Angeles')
  // 浏览器给的是 CLDR 名（印度可能是 Asia/Calcutta），按偏移挑：凡是 :30 / :45 的都必须置灰
  const options = await select.locator('option').evaluateAll((nodes) =>
    nodes.map((node) => ({ label: node.textContent ?? '', disabled: (node as HTMLOptionElement).disabled })))
  const fractional = options.filter((o) => /UTC[+-]\d\d:(30|45)\)/.test(o.label))
  expect(fractional.length).toBeGreaterThan(3)
  for (const option of fractional) {
    expect(option.disabled, option.label).toBe(true)
    expect(option.label).toContain('不支持：与 UTC 的时差不是整小时')
  }
  await expect(select.locator('option[value="Australia/Adelaide"]')).toBeDisabled()
  for (const zone of ['Asia/Shanghai', 'Europe/London', 'America/New_York']) {
    await expect(select.locator(`option[value="${zone}"]`)).toBeEnabled()
  }
})

test('?tab=timezone 直达统计时区页签', async ({ page }) => {
  await prepare(page, 'Asia/Shanghai')
  await page.goto('/admin/settings?tab=timezone')
  await expect(page.getByRole('tab', { name: '时区', exact: true })).toHaveAttribute('aria-selected', 'true')
  await expect(page.getByRole('heading', { name: '统计时区' })).toBeVisible()
})
