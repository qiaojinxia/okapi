import { expect, test } from '@playwright/test'
import type { Page } from '@playwright/test'
import { fileURLToPath } from 'node:url'

const fixtureKey = 'fixture-mcp-login-secret'
const initResult = { protocolVersion: '2025-06-18', serverInfo: { name: 'okapi-mcp', version: '0.1.0' }, capabilities: { tools: {} } }

async function prepare(page: Page, permissions = ['*'], value: unknown = null, language = 'zh-CN') {
  const state = { value, posts: [] as unknown[], calls: [] as { method: string; id: number }[], settingStatus: 200, saveStatus: 200 }
  await page.addInitScript(({ key, language }) => {
    localStorage.setItem('okapi.key', key)
    localStorage.setItem('okapi.lang', language)
    Object.defineProperty(navigator, 'clipboard', { value: {
      writeText: async (text: string) => { (window as unknown as { copiedText: string }).copiedText = text },
    }, configurable: true })
  }, { key: fixtureKey, language })
  await page.route('**/*', async (route) => {
    const request = route.request(), path = new URL(request.url()).pathname
    if (request.isNavigationRequest()) return route.fulfill({ path: fileURLToPath(new URL('../dist/index.html', import.meta.url)), contentType: 'text/html' })
    if (path === '/mcp') {
      expect(request.headers().authorization).toBe(`Bearer ${fixtureKey}`)
      const body = request.postDataJSON()
      state.calls.push(body)
      // 任何业务工具执行均应让测试失败。
      expect(['initialize', 'tools/list']).toContain(body.method)
      return route.fulfill({ json: { jsonrpc: '2.0', id: body.id, result: body.method === 'initialize' ? initResult : { tools: [{ name: 'query_balance', description: 'Account balance.' }] } } })
    }
    if (!/^\/(api|admin|auth)\//.test(path)) return route.continue()
    if (path === '/admin/settings' && request.method() === 'POST') {
      state.posts.push(request.postDataJSON())
      if (state.saveStatus !== 200) return route.fulfill({ status: state.saveStatus, json: { error: { code: 'internal_error' } } })
      state.value = request.postDataJSON().value
      return route.fulfill({ json: { ok: true } })
    }
    expect(request.method()).toBe('GET')
    if (path === '/admin/settings/mcp_write_enabled') throw new Error('单键读取要求 settings.write，接入页应使用脱敏列表接口')
    if (path === '/admin/settings') return route.fulfill({
      status: state.settingStatus, json: state.settingStatus === 200
        ? { data: state.value === null ? [] : [{ key: 'mcp_write_enabled', value: state.value, is_secret: false }] }
        : { error: { code: 'internal_error' } },
    })
    const json = path === '/api/me' ? { user_id: 1, key_id: 1, role: 100, permissions, group: 'default', balance_micro: 0 }
      : path.startsWith('/admin/settings/') ? { value: null } : { data: [] }
    return route.fulfill({ json })
  })
  return state
}

async function open(page: Page) {
  await page.goto('/admin/settings')
  await page.getByRole('tab', { name: 'AI 接入', exact: true }).click()
}
const copied = (page: Page) => page.evaluate(() => (window as unknown as { copiedText: string }).copiedText)

test('MCP 接入：默认只读，配置和三种提示词复制不包含登录凭证，不自动请求 MCP 或写设置', async ({ page }) => {
  const state = await prepare(page)
  await open(page)
  await expect(page.getByText('全站只读模式', { exact: true })).toBeVisible()
  await expect(page.getByRole('switch', { name: 'MCP 写入权限' })).not.toBeChecked()
  const endpoint = 'http://127.0.0.1:4175/mcp'
  await expect(page.getByLabel('MCP 服务地址', { exact: true })).toHaveValue(endpoint)
  await page.getByRole('button', { name: '复制配置', exact: true }).click()
  const config = await copied(page)
  expect(JSON.parse(config)).toEqual({ mcpServers: { okapi: { type: 'http', url: endpoint, headers: { Authorization: 'Bearer <OKAPI_API_KEY>' } } } })
  expect(config).not.toContain(fixtureKey)
  await page.getByRole('button', { name: '复制 MCP 地址', exact: true }).click()
  expect(await copied(page)).toBe(endpoint)
  for (const [task, snippet] of [['inspect', '不执行修复'], ['usage', '不修改余额或定价'], ['manage', '仅执行获准的操作']]) {
    await page.getByLabel('任务模板').selectOption(task)
    await page.getByRole('button', { name: '复制提示词', exact: true }).click()
    expect(await copied(page)).toContain(snippet)
    expect(await copied(page)).not.toContain(fixtureKey)
  }
  expect(state.calls).toEqual([])
  expect(state.posts).toEqual([])
  await expect(page.getByRole('link', { name: '查看审计日志' })).toHaveAttribute('href', '/admin/audit')
})

test('MCP 测试只握手与发现工具，展示实际返回工具和测试边界', async ({ page }) => {
  const state = await prepare(page)
  await open(page)
  await page.getByRole('button', { name: '测试连接', exact: true }).click()
  await expect(page.getByText('握手与工具发现通过', { exact: true })).toBeVisible()
  expect(state.calls.map((call) => call.method)).toEqual(['initialize', 'tools/list'])
  expect(state.posts).toEqual([])
  await expect(page.getByText('本次测试：当前账号可见 1 个工具。')).toBeVisible()
  await page.getByText('查看本次返回的工具列表', { exact: true }).click()
  await expect(page.getByText('query_balance', { exact: true })).toBeVisible()
  await expect(page.getByText(/不代表外部 AI 客户端已连接/)).toBeVisible()
})

for (const variant of ['html', 'rpc-error', 'bad-tools', '401', '403', '404', 'network', 'redirect', 'timeout']) {
  test(`MCP 测试失败不伪装已连接，支持重试：${variant}`, async ({ page }) => {
    const state = await prepare(page)
    if (variant === 'timeout') await page.addInitScript(() => {
      const original = AbortSignal.timeout.bind(AbortSignal)
      AbortSignal.timeout = () => original(200)
    })
    await page.route('**/mcp', async (route) => {
      if (variant === 'network') return route.abort('failed')
      if (variant === 'timeout') return // 不响应，让客户端超时
      if (variant === 'redirect') return route.fulfill({ status: 302, headers: { Location: 'https://example.invalid/mcp' } })
      if (variant === 'html') return route.fulfill({ contentType: 'text/html', body: '<html>frontend fallback</html>' })
      if (/^\d+$/.test(variant)) return route.fulfill({ status: Number(variant), json: { error: { code: 'fixture' } } })
      const body = route.request().postDataJSON()
      return route.fulfill({ json: variant === 'rpc-error'
        ? { jsonrpc: '2.0', id: body.id, error: { code: -32601, message: 'method_not_found' } }
        : { jsonrpc: '2.0', id: body.id, result: body.method === 'initialize' ? initResult : { tools: [{}] } } })
    })
    await open(page)
    await page.getByRole('button', { name: '测试连接', exact: true }).click()
    await expect(page.getByRole('alert')).toContainText('连接测试未通过')
    await expect(page.getByText('握手与工具发现通过', { exact: true })).toHaveCount(0)
    expect(state.posts).toEqual([])
    // 替换失败桩，证明按钮可重试，旧错误清除。
    await page.route('**/mcp', (route) => {
      const body = route.request().postDataJSON()
      return route.fulfill({ json: { jsonrpc: '2.0', id: body.id, result: body.method === 'initialize' ? initResult : { tools: [] } } })
    })
    await page.getByRole('button', { name: '测试连接', exact: true }).click()
    await expect(page.getByText('握手与工具发现通过', { exact: true })).toBeVisible()
    await expect(page.getByText('连接测试未通过', { exact: true })).toHaveCount(0)
  })
}

test('MCP 全站写入开关：取消不写入，确认后保存，保存失败不假装关闭', async ({ page }) => {
  const state = await prepare(page)
  await open(page)
  const toggle = page.getByRole('switch', { name: 'MCP 写入权限' })
  await toggle.click()
  let dialog = page.getByRole('alertdialog', { name: '开启全站 MCP 写入？' })
  await expect(dialog).toContainText('所有具备 mcp.write')
  expect(state.posts).toEqual([])
  await dialog.getByRole('button', { name: '取消', exact: true }).click()
  await expect(toggle).not.toBeChecked()
  await toggle.click()
  await dialog.getByRole('button', { name: '确认开启写入' }).click()
  await expect(toggle).toBeChecked()
  expect(state.posts).toEqual([{ key: 'mcp_write_enabled', value: true }])
  await expect(page.getByText('全站写入已开启', { exact: true })).toBeVisible()
  state.saveStatus = 500
  await toggle.click()
  dialog = page.getByRole('alertdialog', { name: '关闭全站 MCP 写入？' })
  await dialog.getByRole('button', { name: '确认关闭写入' }).click()
  await expect(page.getByRole('alert')).toContainText('服务内部错误')
  await expect(toggle).toBeChecked()
  state.saveStatus = 200
  await toggle.click()
  await dialog.getByRole('button', { name: '确认关闭写入' }).click()
  await expect(toggle).not.toBeChecked()
})

test('MCP 只读管理员不能切换权限，不重置站点既有开启状态', async ({ page }) => {
  const state = await prepare(page, ['settings.read'], true)
  await open(page)
  await expect(page.getByRole('switch', { name: 'MCP 写入权限' })).toBeChecked()
  await expect(page.getByRole('switch', { name: 'MCP 写入权限' })).toBeDisabled()
  await expect(page.getByRole('link', { name: '查看审计日志' })).toHaveCount(0)
  expect(state.posts).toEqual([])
})

for (const status of ['failed', 'malformed']) {
  test(`MCP 未知设置状态禁止写入且不声称只读：${status}`, async ({ page }) => {
    const state = await prepare(page, ['*'], status === 'malformed' ? 'true' : null)
    if (status === 'failed') state.settingStatus = 500
    await open(page)
    await expect(page.getByText('权限状态待确认', { exact: true })).toBeVisible()
    await expect(page.getByRole('alert')).toBeVisible()
    await expect(page.getByRole('switch', { name: 'MCP 写入权限' })).toHaveCount(0)
    expect(state.posts).toEqual([])
  })
}

for (const [width, language] of [[1440, 'zh-CN'], [1920, 'en']] as const) {
  test(`MCP 页面视觉：${width} / ${language} 无横向溢出`, async ({ page }) => {
    await page.setViewportSize({ width, height: 1080 })
    await prepare(page, ['*'], false, language)
    await page.goto('/admin/settings')
    await page.getByRole('tab', { name: language === 'en' ? 'AI access' : 'AI 接入', exact: true }).click()
    const title = page.getByRole('heading', { name: language === 'en' ? '1. Configure the MCP connection' : '1. 配置 MCP 连接' })
    await expect(title).toBeVisible()
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true)
    await page.screenshot({ path: `test-results/mcp-settings-${width}-${language}.png`, fullPage: true, animations: 'disabled' })
  })
}
