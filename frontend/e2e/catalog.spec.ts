import { expect, test } from '@playwright/test'
import type { Page } from '@playwright/test'
import { fileURLToPath } from 'node:url'
import type { PricingModel } from '../src/features/public-pricing/types'
import { compareModels, modelCapabilities, modelPrice, modelVendor, nonnegative } from '../src/features/public-pricing/catalog-data'
import { formatUnitPrice } from '../src/lib/money'

// 仅用于界面回归的展示数据，不是线上报价或能力清单。
const model = (id: string, vendor: string | null, name: string | null, overrides: Partial<PricingModel> = {}): PricingModel => ({
  model: id, vendor, display_name: name, mode: 'ratio', model_ratio: '1', completion_ratio: '4', cache_ratio: '0.25',
  cache_write_ratio: '1', audio_ratio: '1', audio_completion_ratio: '1', image_ratio: '1', per_call_price_micro: null,
  groups: ['default', 'economy'], capabilities: { tools: true, vision: true }, context_window: 128000, max_output: 8192, ...overrides,
})
const models = [
  model('gpt-4.1', 'OpenAI', 'GPT-4.1'),
  model('gpt-4.1-mini', 'OpenAI', 'GPT-4.1 mini', { model_ratio: '0.2', completion_ratio: '4' }),
  model('claude-sonnet-4', 'Anthropic', 'Claude Sonnet 4', { model_ratio: '1.5', completion_ratio: '5', context_window: 200000, cache_write_ratio: '1.25' }),
  model('claude-haiku-3.5', 'Anthropic', 'Claude Haiku 3.5', { model_ratio: '0.4', completion_ratio: '5', context_window: 200000 }),
  model('gemini-2.5-pro', 'Google', 'Gemini 2.5 Pro', { mode: 'tiered', context_window: 1048576, capabilities: { vision: true, reasoning: true, audio: true, video: true } }),
  model('gemini-2.5-flash', 'Google', 'Gemini 2.5 Flash', { model_ratio: '0.15', context_window: 1048576 }),
  model('deepseek-chat', 'DeepSeek', 'DeepSeek V3', { model_ratio: '0.135', capabilities: { tools: true, json: true }, context_window: 65536 }),
  model('deepseek-reasoner', 'DeepSeek', 'DeepSeek R1', { model_ratio: '0.275', capabilities: { reasoning: true }, context_window: 65536 }),
  model('qwen3-32b', 'Alibaba', 'Qwen3 32B', { model_ratio: '0.06', capabilities: { tools: true, reasoning: true }, context_window: 32768 }),
  model('qwen-plus', 'qwen', 'Qwen Plus', { model_ratio: '0.000005', groups: ['economy'] }),
  model('kimi-k2', 'Moonshot', 'Kimi K2', { model_ratio: '0.3' }),
  model('grok-3', 'xAI', 'Grok 3', { groups: [] }),
  model('image-studio', 'Custom Studio', 'Image Studio', { mode: 'per_call', per_call_price_micro: 40000, capabilities: {}, context_window: null, max_output: null }),
  model('private-model-with-a-very-long-canonical-identifier-v2.0', null, null, { model_ratio: null, completion_ratio: null, capabilities: {}, context_window: null, max_output: null, groups: [] }),
]
const groups = [{ code: 'default', name: '标准分组', ratio: '1' }, { code: 'economy', name: '经济分组', ratio: '0.5' }, { code: 'free', name: '内部免费组', ratio: '0' }]

async function prepare(page: Page, data = models, language = 'zh-CN', dark = false) {
  const calls: string[] = []
  const errors: string[] = []
  page.on('pageerror', (error) => errors.push(error.message))
  await page.addInitScript(({ language, dark }) => {
    localStorage.setItem('okapi.lang', language)
    localStorage.setItem('okapi.theme', dark ? 'dark' : 'light')
  }, { language, dark })
  await page.route('**/*', async (route) => {
    const request = route.request(), url = new URL(request.url())
    if (request.isNavigationRequest()) return route.fulfill({ path: fileURLToPath(new URL('../dist/index.html', import.meta.url)), contentType: 'text/html' })
    if (url.pathname.startsWith('/api/') || url.pathname.startsWith('/auth/')) {
      expect(request.method()).toBe('GET')
      calls.push(url.pathname)
      return route.fulfill({ json: url.pathname === '/api/pricing' ? { models: data, groups } : {} })
    }
    expect(url.hostname).toBe('127.0.0.1')
    return route.continue()
  })
  return { calls, errors }
}

test('目录按显式厂商归一，零价、未知价和不同计费单位分开处理', () => {
  expect(modelVendor(model('gpt-private', 'My lab', null))).toEqual({ id: 'custom:my lab', name: 'My lab' })
  expect(modelVendor(model('gpt-private', null, null)).id).toBe('other')
  expect(modelVendor(model('x', 'Qwen', null)).id).toBe(modelVendor(model('y', 'Alibaba', null)).id)
  expect(modelVendor(model('x', '__proto__', null)).name).toBe('__proto__')
  expect(modelCapabilities(model('x', null, null, { capabilities: { tools: false, arbitrary: true } }))).toEqual([])
  expect(modelVendor(model('x', '通义千问', null)).id).toBe('alibaba')
  expect(nonnegative('')).toBeNull()
  expect(nonnegative('NaN')).toBeNull()
  expect(nonnegative('-1')).toBeNull()
  expect(nonnegative('0')).toBe(0)
  expect(modelPrice(models[0], 'input', 0)).toBe(0)
  expect(modelPrice(models[0], 'output', 0.5, '1K')).toBe(4000)
  expect(modelPrice(models[12], 'call', 0.5)).toBe(20000)
  expect(modelPrice(models[12], 'input', 1)).toBeNull()
  expect(modelPrice(models[4], 'input', 1)).toBeNull()
  expect(modelPrice(models[13], 'input', 1)).toBeNull()
  expect(modelPrice(model('x', null, null, { per_call_price_micro: null, mode: 'per_call' }), 'call', 1)).toBeNull()
  expect(modelPrice(model('x', null, null, { audio_ratio: '3', audio_completion_ratio: '2' }), 'audioOut', 0.5)).toBe(6000000)
  expect(formatUnitPrice(modelPrice(models[9], 'input', 1, '1K'), 'en')).toBe('$0.00000001')
  expect(formatUnitPrice(0.001, 'en')).toBe('<$0.00000001')
  expect([models[4], models[12], models[0], models[9]].sort((a, b) => compareModels(a, b, 'input', 1, 'en')).slice(0, 2).map((m) => m.model)).toEqual(['qwen-plus', 'gpt-4.1'])
})

test('桌面厂商图标、搜索和能力筛选清晰可用，无额外请求', async ({ page }) => {
  const { calls, errors } = await prepare(page)
  await page.setViewportSize({ width: 1440, height: 1100 })
  await page.goto('/pricing')
  await expect(page.getByRole('heading', { name: '模型广场', exact: true })).toBeVisible()
  await expect(page.locator('article[data-model]')).toHaveCount(14)
  await expect.poll(() => page.locator('img[src^="/vendor-icons/"]').evaluateAll((images) => images.every((image) => (image as HTMLImageElement).naturalWidth > 0))).toBe(true)
  await page.screenshot({ path: 'test-results/catalog-desktop.png', animations: 'disabled' })
  const vendors = page.getByRole('navigation', { name: '模型厂商' })
  await vendors.getByRole('button', { name: /Alibaba/ }).click()
  await expect(page.locator('article[data-model]')).toHaveCount(2)
  await expect(page).toHaveURL(/vendor=alibaba/)
  await page.getByRole('searchbox').fill('PLUS')
  await expect(page.locator('article[data-model]')).toHaveCount(1)
  await page.getByRole('searchbox').fill('not found')
  await expect(page.getByText('换个关键词，或清除厂商和能力筛选。')).toBeVisible()
  await page.getByRole('button', { name: '清除筛选' }).first().click()
  await page.getByLabel('模型能力', { exact: true }).selectOption('reasoning')
  await expect(page.locator('article[data-model]')).toHaveCount(3)
  await page.getByLabel('仅看已接入').check()
  await expect(page.locator('article[data-model]')).toHaveCount(3)
  expect(calls.filter((c) => c === '/api/pricing')).toHaveLength(1)
  expect(errors).toEqual([])
})

test('分组联动按次费用和零价，详情复制与模拟器校验，关闭恢复焦点', async ({ page, context }) => {
  await context.grantPermissions(['clipboard-read', 'clipboard-write'])
  await prepare(page)
  await page.goto('/pricing?vendor=custom%3Acustom%20studio')
  const card = page.locator('article[data-model="image-studio"]')
  await page.getByLabel('按分组查看').selectOption('economy')
  await expect(card).toContainText('US$0.02')
  await card.getByRole('button', { name: '价格与详情' }).click()
  const drawer = page.getByRole('dialog')
  await expect(drawer).toBeVisible()
  await drawer.getByRole('button', { name: '复制模型 ID' }).click()
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe('image-studio')
  await drawer.getByLabel('调用次数').fill('3')
  await expect(drawer.locator('[aria-live="polite"]')).toContainText('US$0.06')
  await drawer.getByLabel('按分组查看').selectOption('free')
  await expect(drawer.locator('[aria-live="polite"]')).toContainText('US$0.00')
  await drawer.getByLabel('调用次数').fill('-1')
  await expect(drawer.getByRole('alert')).toBeVisible()
  await page.keyboard.press('Escape')
  await expect(drawer).toHaveCount(0)
  await expect(card.getByRole('button', { name: '价格与详情' })).toBeFocused()
  await expect(card).toContainText('US$0.00')
  await expect(card).toContainText('该分组不可用')
})

test('深链接保持表格、单位和筛选；Token 详情计算与未知规格区分', async ({ page }) => {
  await prepare(page)
  await page.goto('/pricing?vendor=openai&view=table&unit=1K&group=economy&sort=input')
  await expect(page.locator('tbody tr')).toHaveCount(2)
  await expect(page.locator('tbody tr').first()).toContainText('GPT-4.1 mini')
  await expect(page.locator('tbody tr').first()).toContainText('US$0.0002')
  await page.reload()
  await expect(page.getByRole('button', { name: '1K tokens', exact: true })).toHaveAttribute('aria-pressed', 'true')
  await page.getByRole('button', { name: '查看 GPT-4.1 详情', exact: true }).first().click()
  const drawer = page.getByRole('dialog')
  await expect(drawer).toContainText('128,000 tokens')
  await drawer.getByLabel('其中缓存读取').fill('600')
  await drawer.getByLabel('其中缓存写入').fill('500')
  await expect(drawer.getByRole('alert')).toBeVisible()
  await drawer.getByLabel('其中缓存写入').fill('400')
  await expect(drawer.getByRole('alert')).toHaveCount(0)
  await expect(drawer.locator('[aria-live="polite"]')).toContainText('US$0.00255')
  await page.goBack()
  await expect(drawer).toHaveCount(0)
  await expect(page.locator('tbody tr')).toHaveCount(2)
})

test('阶梯价不冒充固定价，未声明能力不推断；小额 1K 单价不归零', async ({ page }) => {
  await prepare(page)
  await page.goto('/pricing?model=gemini-2.5-pro')
  const drawer = page.getByRole('dialog')
  await expect(drawer).toContainText('按规则计算')
  await expect(drawer.getByText('定价模拟器')).toHaveCount(0)
  await page.keyboard.press('Escape')
  await page.goto('/pricing?model=private-model-with-a-very-long-canonical-identifier-v2.0')
  await expect(drawer.getByText('未提供', { exact: true })).toHaveCount(2)
  await expect(drawer).toContainText('尚未提供模型能力信息')
  await expect(drawer).not.toContainText('图像理解')
  await page.keyboard.press('Escape')
  await page.goto('/pricing?vendor=alibaba&unit=1K&q=plus')
  await expect(page.locator('article')).toContainText('US$0.00000001')
})

test('移动端深色英文目录与详情不横向溢出', async ({ page }) => {
  const { errors } = await prepare(page, models, 'en', true)
  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto('/pricing?vendor=anthropic')
  await expect(page.locator('article')).toHaveCount(2)
  await expect(page.locator('html')).toHaveClass(/dark/)
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  expect((await page.locator('article').first().boundingBox())!.y).toBeLessThan(600)
  await page.screenshot({ path: 'test-results/catalog-mobile-dark.png', fullPage: true, animations: 'disabled' })
  await page.getByRole('button', { name: /Filters & pricing/ }).click()
  await page.getByLabel('View as group').selectOption('economy')
  await page.getByRole('button', { name: /Filters & pricing/ }).click()
  await expect(page.getByLabel('View as group')).not.toBeVisible()
  await expect(page.locator('article').first()).toContainText('$0.40')
  await page.locator('article').first().getByRole('button', { name: 'Pricing & details' }).click()
  await expect(page.getByRole('dialog')).toBeVisible()
  expect(await page.getByRole('dialog').evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true)
  await page.screenshot({ path: 'test-results/catalog-detail-dark.png', animations: 'disabled' })
  expect(errors).toEqual([])
})

test('大目录按页浏览，页码可刷新和返回，筛选及页容量变化后复位', async ({ page }) => {
  await prepare(page, Array.from({ length: 55 }, (_, i) => model(`model-${i}`, i < 28 ? 'OpenAI' : 'Anthropic', `Model ${i}`)))
  await page.goto('/pricing')
  await expect(page.locator('article')).toHaveCount(24)
  await page.getByRole('button', { name: '下一页' }).click()
  await expect(page.locator('article')).toHaveCount(24)
  await expect(page).toHaveURL(/page=2/)
  await page.reload()
  await expect(page.getByRole('button', { name: '2', exact: true })).toHaveAttribute('aria-current', 'page')
  await page.getByRole('button', { name: '下一页' }).click()
  await expect(page.locator('article')).toHaveCount(7)
  await expect(page.getByRole('button', { name: '下一页' })).toBeDisabled()
  await page.goBack()
  await expect(page.locator('article')).toHaveCount(24)
  await page.getByRole('navigation', { name: '模型厂商' }).getByRole('button', { name: /Anthropic/ }).click()
  await expect(page.locator('article')).toHaveCount(24)
  await expect(page.getByRole('button', { name: '上一页' })).toBeDisabled()
  await page.getByLabel('每页数量').selectOption('12')
  await expect(page.locator('article')).toHaveCount(12)
  await page.getByRole('button', { name: '下一页' }).click()
  await page.getByLabel('每页数量').selectOption('48')
  await expect(page.locator('article')).toHaveCount(27)
  await expect(page.getByRole('button', { name: '下一页' })).toHaveCount(0)
  await page.goto('/pricing?page=999&pageSize=12')
  await expect(page.locator('article')).toHaveCount(7)
  await expect(page.getByRole('button', { name: '5', exact: true })).toHaveAttribute('aria-current', 'page')
})

test('加载、失败与空目录各有明确状态', async ({ page }) => {
  await prepare(page, [])
  let release: () => void = () => undefined
  const gate = new Promise<void>((resolve) => { release = resolve })
  await page.route('**/api/pricing', async (route) => { await gate; await route.fulfill({ status: 403, json: { error: { code: 'permission_denied' } } }) })
  await page.goto('/pricing')
  await expect(page.getByRole('status')).toBeVisible()
  await expect(page.getByText('本站暂未发布模型价格。')).toHaveCount(0)
  release()
  await expect(page.getByRole('alert')).toBeVisible()
  await page.unroute('**/api/pricing')
  await page.getByRole('button', { name: '重试' }).click()
  await expect(page.getByText('本站暂未发布模型价格。')).toBeVisible()
})

test('调用示例可编辑与复制，地址指向网关，密钥不进入代码或链接', async ({ page, context }) => {
  await context.grantPermissions(['clipboard-read', 'clipboard-write'])
  const { calls, errors } = await prepare(page)
  await page.addInitScript(() => localStorage.setItem('okapi.key', 'private-session-key-not-for-snippets'))
  await page.goto('/pricing?vendor=openai')
  await page.locator('article[data-model="gpt-4.1"]').getByRole('button', { name: '调用示例' }).click()
  const drawer = page.getByRole('dialog')
  await expect(drawer.getByRole('tab', { name: '调用示例' })).toHaveAttribute('aria-selected', 'true')
  await expect(page).toHaveURL(/tab=code/)
  await expect(drawer.getByLabel('接口基础地址（Base URL）')).toHaveValue('http://127.0.0.1:8080/v1')
  await drawer.getByRole('button', { name: '复制请求 URL' }).click()
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe('http://127.0.0.1:8080/v1/chat/completions')
  await drawer.getByLabel('接口基础地址（Base URL）').fill('https://gateway.example.com/api/v1/')
  await drawer.getByText('编辑请求输入', { exact: true }).click()
  await drawer.getByLabel('请求输入', { exact: true }).fill('Tell me about "quoted" text and $HOME.\n你好')
  await drawer.getByLabel('流式响应').check()
  await drawer.getByRole('button', { name: '复制示例', exact: true }).click()
  const command = await page.evaluate(() => navigator.clipboard.readText())
  expect(command).toContain('https://gateway.example.com/api/v1/chat/completions')
  expect(command).toContain('--no-buffer')
  expect(command).toContain('"stream": true')
  expect(command).toContain('"model": "gpt-4.1"')
  expect(command).not.toContain('private-session-key-not-for-snippets')
  expect(page.url()).not.toContain('private-session-key-not-for-snippets')
  await drawer.getByText('设置 API Key', { exact: true }).click()
  await drawer.getByRole('button', { name: '复制环境变量命令' }).click()
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe("export OKAPI_API_KEY='YOUR_API_KEY'")
  await drawer.getByRole('tab', { name: '价格与详情' }).click()
  await drawer.getByRole('tab', { name: '调用示例' }).click()
  await expect(drawer.getByLabel('请求输入', { exact: true })).toHaveValue('Tell me about "quoted" text and $HOME.\n你好')
  await drawer.getByLabel('接口模板').selectOption('embeddings')
  await expect(drawer.getByLabel('流式响应')).toHaveCount(0)
  await drawer.getByRole('group', { name: '示例语言' }).getByRole('button', { name: 'JSON', exact: true }).click()
  const body = JSON.parse(await drawer.getByLabel('生成的调用示例').innerText())
  expect(body).toEqual({ model: 'gpt-4.1', input: 'Tell me about "quoted" text and $HOME.\n你好' })
  const download = page.waitForEvent('download')
  await drawer.getByRole('button', { name: '下载 .sh 脚本' }).click()
  expect((await download).suggestedFilename()).toBe('okapi-embeddings.sh')
  await drawer.getByLabel('接口基础地址（Base URL）').fill('https://user:password@example.com/v1?key=secret')
  await expect(drawer.getByRole('alert')).toBeVisible()
  await expect(drawer.getByRole('button', { name: '复制示例' })).toBeDisabled()
  await expect(drawer.getByRole('button', { name: '下载 .sh 脚本' })).toBeDisabled()
  await page.reload()
  await expect(drawer.getByRole('tab', { name: '调用示例' })).toHaveAttribute('aria-selected', 'true')
  expect(calls.filter((path) => path === '/api/pricing')).toHaveLength(2)
  expect(errors).toEqual([])
  await page.screenshot({ path: 'test-results/catalog-api-examples.png', animations: 'disabled' })
})

test('手机调用页签支持键盘与窄屏，已声明向量模型使用向量模板', async ({ page }) => {
  const { errors } = await prepare(page, [model('my-embedding-model', 'OpenAI', 'Text embeddings', { capabilities: { embedding: true } })], 'en', true)
  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto('/pricing?model=my-embedding-model&tab=code')
  const drawer = page.getByRole('dialog')
  await expect(drawer.getByLabel('Endpoint template')).toHaveValue('embeddings')
  await expect(drawer.getByRole('tab', { name: 'API examples' })).toBeFocused()
  await page.keyboard.press('ArrowLeft')
  await page.keyboard.press('Enter')
  await expect(drawer.getByRole('tab', { name: 'Pricing & details' })).toHaveAttribute('aria-selected', 'true')
  await page.keyboard.press('ArrowRight')
  await page.keyboard.press('Enter')
  await expect(drawer.getByRole('tab', { name: 'API examples' })).toHaveAttribute('aria-selected', 'true')
  expect(await drawer.evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true)
  await drawer.getByRole('group', { name: 'Example language' }).getByRole('button', { name: 'JavaScript' }).click()
  await expect(drawer.getByLabel('Generated API example')).toContainText('process.env.OKAPI_API_KEY')
  await page.screenshot({ path: 'test-results/catalog-api-mobile.png', animations: 'disabled' })
  expect(errors).toEqual([])
})
