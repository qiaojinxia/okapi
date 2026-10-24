import { test, expect, type Page } from '@playwright/test'
import { fileURLToPath } from 'node:url'
import { deviates, mergeCatalogSpecs, NOTE_CODES, priceRatio, type CatalogHit, type CatalogSuggestion } from '../src/features/models/catalog'
import { metadataDraft, validLimit } from '../src/features/models/model-config'
import zhBase from '../src/locales/zh-CN'
import { withModelEditorZh } from '../src/locales/model-editor-zh'
import en from '../src/locales/en'

const zh = withModelEditorZh(zhBase)

const snapshot = { dataset: 'litellm', commit: 'a'.repeat(40), generated_at: '2026-10-24', source: 'https://example.test/litellm' }

const claude: CatalogSuggestion = {
  key: 'claude-catalog-x', provider: 'anthropic', matched: 'exact', mode: 'chat', kind: 'chat', billing: 'ratio',
  input_per_1m: '3', output_per_1m: '15', cache_read_ratio: '0.1', cache_write_ratio: '1.25', cache_write_1h_ratio: '2',
  per_call_usd: null, context_window: 200000, max_output: 64000,
  input_modalities: ['text', 'image'], output_modalities: ['text'],
  capabilities: { vision: true, tools: true, reasoning: true },
  notes: [
    { code: 'long_context', params: { above_tokens: '200000', input_per_1m: '6', output_per_1m: '22.5' } },
    { code: 'service_tiers', params: { tiers: 'batch,priority' } },
  ],
  deprecated: '2020-01-01', source_url: 'https://example.test/pricing',
  alternatives: [{ key: 'vertex_ai/claude-catalog-x', provider: 'vertex_ai-anthropic_models', input_per_1m: '3', output_per_1m: '15' }],
}

const image: CatalogSuggestion = {
  ...claude, key: 'image-catalog-x', provider: 'openai', mode: 'image_generation', kind: 'image_generation', billing: 'per_call',
  input_per_1m: null, output_per_1m: null, cache_read_ratio: null, cache_write_ratio: null, cache_write_1h_ratio: null,
  per_call_usd: '0.04', context_window: null, max_output: null, input_modalities: ['text'], output_modalities: ['image'],
  capabilities: {}, notes: [], deprecated: null, alternatives: [],
}

const embedding: CatalogSuggestion = {
  ...claude, key: 'embed-catalog-x', mode: 'embedding', kind: 'embedding', input_per_1m: '0.02', output_per_1m: null,
  cache_read_ratio: null, cache_write_ratio: null, cache_write_1h_ratio: null, context_window: 8191, max_output: null,
  capabilities: { embedding: true }, notes: [], deprecated: null, alternatives: [],
}

const catalog: Record<string, CatalogSuggestion> = {
  'claude-catalog-x': claude, 'image-catalog-x': image, 'embed-catalog-x': embedding, 'catalog-only-model': { ...claude, key: 'catalog-only-model', notes: [], deprecated: null },
}

const hits: CatalogHit[] = [
  { id: 'catalog-only-model', provider: 'openai', mode: 'chat', kind: 'chat', input_per_1m: '1', output_per_1m: '2', listings: 2 },
]

// 预设里已有的名字（gpt-6-astra）故意排在目录独有的名字后面：下拉里仍应是预设在前、且不重复
const overlapHits: CatalogHit[] = [
  { id: 'gpt-6-catalog-extra', provider: 'openai', mode: 'chat', kind: 'chat', input_per_1m: '1', output_per_1m: '2', listings: 1 },
  { id: 'gpt-6-astra', provider: 'openai', mode: 'chat', kind: 'chat', input_per_1m: '1', output_per_1m: '2', listings: 3 },
]

async function setup(page: Page) {
  const posts: Record<string, unknown>[] = []
  const lookups: string[] = []
  await page.addInitScript(() => { localStorage.setItem('okapi.key', 'fixture-model-catalog'); localStorage.setItem('okapi.lang', 'zh-CN') })
  await page.route('**/*', (route) => {
    const request = route.request(), url = new URL(request.url()), path = url.pathname
    if (request.isNavigationRequest()) return route.fulfill({ path: fileURLToPath(new URL('../dist/index.html', import.meta.url)), contentType: 'text/html' })
    if (!/^\/(api|admin|auth)\//.test(path)) return route.continue()
    let json: unknown = { data: [] }
    if (path === '/api/me') json = { user_id: 1, key_id: 1, role: 100, permissions: ['*'], group: 'default', balance_micro: 0 }
    if (path === '/api/me/groups') json = { current: 'default', data: [{ code: 'default' }] }
    if (path === '/admin/models') {
      if (request.method() === 'POST') { posts.push(request.postDataJSON()); json = { model_id: 1, requires_publish: true } }
      else json = { data: [], total: 0, unpriced: 0, base_price_per_1m_micro: 2_000_000 }
    }
    if (path === '/admin/pricing/catalog/lookup') {
      const model = url.searchParams.get('model') ?? ''
      lookups.push(model)
      if (model === 'boom-model') return route.fulfill({ status: 500, json: { error: { code: 'internal_error' } } })
      json = { suggestion: catalog[model] ?? null, snapshot }
    }
    if (path === '/admin/pricing/catalog/search') {
      const q = url.searchParams.get('q') ?? ''
      json = { data: q.startsWith('catalog-only') ? hits : q.startsWith('gpt-6') ? overlapHits : [] }
    }
    return route.fulfill({ json })
  })
  await page.goto('/admin/pricing')
  return { posts, lookups }
}

async function openCreate(page: Page) {
  await page.getByRole('button', { name: '新建模型', exact: true }).click()
  const d = page.getByRole('dialog')
  // 抽屉打开后会把焦点移到首个控件；等它落定再输入，否则焦点跳走会收起候选
  await expect(d.locator(':focus')).toHaveCount(1)
  return d
}

test('价格偏差只在差一倍以上或不到一半时提示；不可比较的输入不提示', () => {
  expect(priceRatio('3', '3')).toBe(1)
  expect(priceRatio('6', '3')).toBe(2)
  expect(priceRatio('1.5', '3')).toBe(0.5)
  for (const missing of [null, undefined, '', '0']) expect(priceRatio('3', missing)).toBeNull()
  for (const bad of ['', '0', 'abc', '-1', '1e3', '3.']) expect(priceRatio(bad, '3')).toBeNull()
  expect(deviates(priceRatio('30', '3'))).toBe(true)
  expect(deviates(priceRatio('6', '3'))).toBe(true)
  expect(deviates(priceRatio('0.3', '3'))).toBe(true)
  expect(deviates(priceRatio('1.5', '3'))).toBe(true)
  // 常见的加价与折扣不提醒
  for (const ordinary of ['3', '3.6', '4.5', '5.9', '1.6', '2']) expect(deviates(priceRatio(ordinary, '3')), ordinary).toBe(false)
  expect(deviates(null)).toBe(false)
})

test('带入规格只补空项：管理员填过的类型 / 模态 / 能力 / 限制一概不动，未知模态与超界限制丢弃', () => {
  const filled = { ...metadataDraft(), kind: 'completion', input_modalities: ['audio'], capabilities: { vision: false }, context_window: '4096' }
  const merged = mergeCatalogSpecs(filled, claude)
  expect(merged.kind).toBe('completion')
  expect(merged.input_modalities).toEqual(['audio'])
  expect(merged.output_modalities).toEqual(['text'])
  expect(merged.capabilities).toEqual({ vision: false, tools: true, reasoning: true })
  expect(merged.context_window).toBe('4096')
  expect(merged.max_output).toBe('64000')
  const fresh = mergeCatalogSpecs(metadataDraft(), {
    ...claude, input_modalities: ['text', 'pdf', 'image'], capabilities: { vision: true, not_a_capability: true, web_search: false }, context_window: 3_000_000_000,
  })
  expect(fresh.input_modalities).toEqual(['text', 'image'])
  expect(fresh.capabilities).toEqual({ vision: true, web_search: false })
  expect(fresh.context_window).toBe('')
  expect(validLimit(fresh.context_window) && validLimit(fresh.max_output)).toBe(true)
  expect(mergeCatalogSpecs(metadataDraft(), embedding)).toMatchObject({ kind: 'embedding', context_window: '8191', max_output: '' })
})

test('后端会给出的备注码在中英文案里都有，前端认得的码也不多不少', () => {
  for (const locale of [zh, en]) {
    const copy: Record<string, unknown> = locale.admin.modelCatalog
    expect(Object.keys(locale.admin.modelCatalog.notes).sort()).toEqual([...NOTE_CODES].sort())
    for (const [key, text] of Object.entries(copy)) expect(text, key).toBeTruthy()
  }
  // 中英文案的键一一对应
  expect(Object.keys(zh.admin.modelCatalog).sort()).toEqual(Object.keys(en.admin.modelCatalog).sort())
})

test('目录建议卡：价格、缓存倍率、规格和注意事项；填入后保存的草稿与建议一致', async ({ page }) => {
  const { posts } = await setup(page)
  const d = await openCreate(page)
  await d.locator('#m-name').fill('claude-catalog-x')
  const card = d.getByTestId('model-catalog-suggestion')
  await expect(card).toBeVisible()
  await expect(d.getByTestId('model-catalog-price')).toHaveText('输入 $3 / 输出 $15 · USD / 1M')
  await expect(card).toContainText('缓存读 0.1× · 缓存写 1.25× · 1 小时缓存写 2×')
  await expect(card).toContainText('输入上限 200,000 · 输出上限 64,000')
  await expect(card).toContainText('超过 200000 token 的请求另有加价：输入 $6 / 输出 $22.5')
  await expect(card).toContainText('batch,priority')
  await expect(card).toContainText('已于 2020-01-01 弃用')
  await expect(card).toContainText('claude-catalog-x · anthropic')
  await expect(card.getByRole('link', { name: /价格来源/ })).toHaveAttribute('href', 'https://example.test/pricing')
  await expect(card).toContainText('快照 2026-10-24')
  await card.getByText('同名条目还有').click()
  await expect(card).toContainText('vertex_ai/claude-catalog-x · vertex_ai-anthropic_models · $3 / $15')

  // 建议只在点击后进表单：这时价格输入仍是默认值
  await expect(d.locator('#price-input')).not.toHaveValue('3')
  await card.getByRole('button', { name: '填入建议价' }).click()
  await expect(d.locator('#price-input')).toHaveValue('3')
  await expect(d.locator('#price-output')).toHaveValue('15')
  await card.getByRole('button', { name: '带入规格' }).click()

  await d.locator('#model-advanced-section > summary').click()
  await d.locator('#model-cache-section > summary').click()
  await expect(d.locator('#ax-cache_ratio')).toHaveValue('0.1')
  await expect(d.locator('#ax-cache_write_ratio')).toHaveValue('1.25')
  await d.locator('#model-cache-ttl-section > summary').click()
  await expect(d.locator('#ind-cache_write_1h')).toHaveValue('2')

  await d.getByRole('button', { name: '保存', exact: true }).click()
  await expect(d).toHaveCount(0)
  expect(posts[0]).toMatchObject({
    model_name: 'claude-catalog-x', pricing_mode: 'ratio', model_ratio: '1.5', completion_ratio: '5',
    cache_ratio: '0.1', cache_write_ratio: '1.25', modality_ratios: { cache_write_1h: '2' },
    metadata: { kind: 'chat', context_window: 200000, max_output: 64000, input_modalities: ['text', 'image'], output_modalities: ['text'],
      capabilities: { vision: true, tools: true, reasoning: true } },
  })
})

test('嵌入模型没有输出价时只填输入价，输出价保持管理员原来的值', async ({ page }) => {
  const { posts } = await setup(page)
  const d = await openCreate(page)
  await d.locator('#m-name').fill('embed-catalog-x')
  const card = d.getByTestId('model-catalog-suggestion')
  await expect(d.getByTestId('model-catalog-price')).toHaveText('输入 $0.02 · USD / 1M')
  await d.locator('#price-output').fill('0.5')
  await card.getByRole('button', { name: '填入建议价' }).click()
  await expect(d.locator('#price-input')).toHaveValue('0.02')
  await expect(d.locator('#price-output')).toHaveValue('0.5')
  await d.getByRole('button', { name: '保存', exact: true }).click()
  await expect(d).toHaveCount(0)
  expect(posts[0]).toMatchObject({ model_name: 'embed-catalog-x', model_ratio: '0.01', completion_ratio: '25' })
})

test('按次计价的目录条目切到按次价；按阶梯计价时不能硬套', async ({ page }) => {
  const { posts } = await setup(page)
  const d = await openCreate(page)
  await d.locator('#m-name').fill('image-catalog-x')
  const card = d.getByTestId('model-catalog-suggestion')
  await expect(d.getByTestId('model-catalog-price')).toHaveText('按次价 $0.04 / 次')
  await card.getByRole('button', { name: '填入按次价' }).click()
  await expect(d.locator('#m-pricing-mode')).toHaveValue('per_call')
  await expect(d.locator('#m-call-price')).toHaveValue('0.04')
  await d.locator('#m-pricing-mode').selectOption('tiered')
  await expect(card.getByRole('button', { name: '填入按次价' })).toBeDisabled()
  await d.locator('#m-pricing-mode').selectOption('per_call')
  await expect(card.getByRole('button', { name: '填入按次价' })).toBeEnabled()
  await d.getByRole('button', { name: '保存', exact: true }).click()
  await expect(d).toHaveCount(0)
  expect(posts[0]).toMatchObject({ model_name: 'image-catalog-x', pricing_mode: 'per_call', per_call_price_micro: 40000 })
})

test('填的价与建议价差一倍以上时提醒，改回正常范围提示消失；提醒不拦保存', async ({ page }) => {
  const { posts } = await setup(page)
  const d = await openCreate(page)
  await d.locator('#m-name').fill('claude-catalog-x')
  const card = d.getByTestId('model-catalog-suggestion')
  await card.getByRole('button', { name: '填入建议价' }).click()
  await expect(d.getByTestId('price-deviation')).toHaveCount(0)
  await d.locator('#price-input').fill('30')
  await expect(d.getByTestId('price-deviation')).toContainText('输入价是目录建议价的 10×')
  await d.locator('#price-output').fill('1')
  await expect(d.getByTestId('price-deviation')).toContainText('输出价是目录建议价的 0.07×')
  await d.locator('#price-input').fill('3.6')
  await d.locator('#price-output').fill('15')
  await expect(d.getByTestId('price-deviation')).toHaveCount(0)
  await d.locator('#price-input').fill('30')
  await d.getByRole('button', { name: '保存', exact: true }).click()
  await expect(d).toHaveCount(0)
  expect(posts[0]).toMatchObject({ model_name: 'claude-catalog-x', model_ratio: '15' })
})

test('目录没有的名字、目录查询失败、选了人工核对的预设：都不出建议卡，表单照常可用', async ({ page }) => {
  const { posts, lookups } = await setup(page)
  const d = await openCreate(page)
  await d.locator('#m-name').fill('my-private-model')
  await expect(d.getByTestId('model-catalog-none')).toContainText('目录里没有这个名字')
  await expect(d.getByTestId('model-catalog-suggestion')).toHaveCount(0)

  await d.locator('#m-name').fill('boom-model')
  await expect(d.getByTestId('model-catalog-failed')).toContainText('目录查询失败')
  await expect(d.getByTestId('model-catalog-none')).toHaveCount(0)

  // 预设优先：选了预设之后不再问目录（也不显示目录卡）
  await d.locator('#m-name').fill('gpt-6-astra')
  await d.getByRole('option').filter({ hasText: 'gpt-6-astra' }).first().click()
  await expect(d.getByTestId('model-preset-summary')).toBeVisible()
  await expect(d.getByTestId('model-catalog-suggestion')).toHaveCount(0)
  await expect(d.getByTestId('model-catalog-none')).toHaveCount(0)
  const before = lookups.length
  await page.waitForTimeout(700)
  expect(lookups.length).toBe(before)

  await d.locator('#m-name').fill('my-private-model')
  await d.getByRole('button', { name: '保存', exact: true }).click()
  await expect(d).toHaveCount(0)
  expect(posts[0]).toMatchObject({ model_name: 'my-private-model' })
})

test('名称联想：目录命中出现在候选里，选中后带出建议；快速连打不会每个字符都查', async ({ page }) => {
  const { lookups } = await setup(page)
  const d = await openCreate(page)
  const name = d.locator('#m-name')
  await name.pressSequentially('catalog-only', { delay: 20 })
  const option = d.getByRole('option').filter({ hasText: 'catalog-only-model' })
  await expect(option).toBeVisible()
  await expect(option).toContainText('openai · $1 / $2')
  // 连续输入只在停顿后查一次，不是每个字符一次
  await expect.poll(() => lookups.length).toBe(1)
  expect(lookups).toEqual(['catalog-only'])
  await option.click()
  await expect(name).toHaveValue('catalog-only-model')
  await expect(d.getByTestId('model-catalog-suggestion')).toBeVisible()
  await expect(d.getByTestId('model-catalog-price')).toHaveText('输入 $3 / 输出 $15 · USD / 1M')
})

test('名称联想：人工核对过的预设排在目录联想前面，同一个 ID 只出现一次且以预设为准', async ({ page }) => {
  await setup(page)
  const d = await openCreate(page)
  await d.locator('#m-name').pressSequentially('gpt-6', { delay: 20 })
  const options = d.getByRole('option')
  await expect(options.filter({ hasText: 'gpt-6-catalog-extra' })).toBeVisible()
  // 目录也列了 gpt-6-astra，但下拉里只有预设那一条：带显示名与厂商，不带目录的价格
  const astra = options.filter({ hasText: 'gpt-6-astra' })
  await expect(astra).toHaveCount(1)
  await expect(astra).toContainText('GPT-6 Astra')
  await expect(astra).not.toContainText('$1 / $2')
  const texts = await options.allTextContents()
  expect(texts.findIndex((text) => text.includes('gpt-6-astra'))).toBeLessThan(texts.findIndex((text) => text.includes('gpt-6-catalog-extra')))
})

test('编辑已有模型：价格与目录差得多时同样提醒（校验基准）', async ({ page }) => {
  const { lookups } = await setup(page)
  await page.route(/\/admin\/models(\?|$)/, (route) => {
    if (route.request().method() !== 'GET') return route.fallback()
    return route.fulfill({ json: { data: [{
      model_name: 'claude-catalog-x', display_name: null, vendor: 'Anthropic', status: 1, capabilities: {}, context_window: null, max_output: null,
      pricing_mode: 'ratio', model_ratio: '15', completion_ratio: '5', cache_ratio: '1', cache_write_ratio: '1', audio_ratio: '1', audio_completion_ratio: '1', image_ratio: '1',
      tier_expr: null, per_call_price_micro: null, fallback_models: [], modality_ratios: {}, tier_ratios: {},
    }], total: 1, unpriced: 0, base_price_per_1m_micro: 2_000_000 } })
  })
  await page.reload()
  await page.getByRole('row').filter({ hasText: 'claude-catalog-x' }).getByRole('button', { name: '编辑', exact: true }).click()
  const d = page.getByRole('dialog')
  await expect(d.getByTestId('model-catalog-suggestion')).toBeVisible()
  await expect(d.getByTestId('price-deviation')).toContainText('输入价是目录建议价的 10×')
  expect(lookups).toContain('claude-catalog-x')
})
