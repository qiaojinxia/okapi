import { test, expect, type Page } from '@playwright/test'
import { fileURLToPath } from 'node:url'
import { metadataDraft, metadataPayload, priceFromMicro, pricesFromRatios, ratiosFromPrices, usdMicro, validLimit, validRatio } from '../src/features/models/model-config'
import { billingLines, type LogRow } from '../src/features/logs/types'
import { modelPrice } from '../src/features/public-pricing/catalog-data'
import type { ModelListRow } from '../src/features/models/types'
import type { PricingModel } from '../src/features/public-pricing/types'
import { MODEL_PRESETS, findModelPreset, presetMetadata, presetCache, referencePriceAvailable } from '../src/features/models/model-presets'
import { CAPABILITY_KEYS, MODEL_KINDS, TEXT_AXES, MODAL_AXES, INDEPENDENT_AXES } from '../src/features/models/types'
import { pricingDisabledReason } from '../src/features/models/model-pricing-availability'
import zhBase from '../src/locales/zh-CN'
import { withModelEditorZh } from '../src/locales/model-editor-zh'
import en from '../src/locales/en'

const zh = withModelEditorZh(zhBase)

const existing: ModelListRow = {
  model_name: 'custom-vision', display_name: '视觉助手', vendor: 'Example', status: 1,
  capabilities: { vision: true, tools: false }, context_window: 128000, max_output: 8192,
  catalog_config: { kind: 'chat', description: '人工声明', input_modalities: ['text', 'image'], output_modalities: ['text'] },
  pricing_mode: 'per_call', per_call_price_micro: 12345,
  model_ratio: '1', completion_ratio: '4', cache_ratio: '0.1', cache_write_ratio: '1.25',
  audio_ratio: '1', audio_completion_ratio: '1', image_ratio: '1', tier_expr: null,
  modality_ratios: { cache_write_5m: '1.25', cache_write_1h: '2', audio_cache_read: '0.3' },
  tier_ratios: { flex: '0.5', priority: '2' }, fallback_models: [],
}

async function setup(page: Page, models: ModelListRow[] = [], base = 2_000_000) {
  const posts: Record<string, unknown>[] = []
  await page.addInitScript(() => { localStorage.setItem('okapi.key', 'fixture-model-config'); localStorage.setItem('okapi.lang', 'zh-CN') })
  await page.route('**/*', (route) => {
    const request = route.request(), path = new URL(request.url()).pathname
    if (request.isNavigationRequest()) return route.fulfill({ path: fileURLToPath(new URL('../dist/index.html', import.meta.url)), contentType: 'text/html' })
    if (!/^\/(api|admin|auth)\//.test(path)) return route.continue()
    let json: unknown = { data: [] }
    if (path === '/api/me') json = { user_id: 1, key_id: 1, role: 100, permissions: ['*'], group: 'default', balance_micro: 0 }
    if (path === '/api/me/groups') json = { current: 'default', data: [{ code: 'default' }] }
    if (path === '/api/pricing') {
      const query = new URL(request.url()).searchParams
      const filtered = models.filter((m) => !query.has('model') || query.get('model') === m.model_name)
      const limit = Number(query.get('limit') ?? 100), offset = Number(query.get('offset') ?? 0)
      const count = Math.min(limit, Math.max(0, filtered.length - offset)), more = offset + count < filtered.length
      json = { models: filtered.slice(offset, offset + limit).map((m) => ({ ...m, model: m.model_name, mode: m.pricing_mode, base_price_per_1m_micro: base, groups: ['default'] })), groups: [{ code: 'default', name: null, ratio: '1', is_default: true }], total: filtered.length, limit, offset, has_more: more, next_offset: more ? offset + count : null, groups_page: { total: 1, limit: 100, offset: 0, has_more: false, next_offset: null }, pricing_epoch: 1 }
    }
    if (path === '/api/pricing/stats') {
      const vendors = [...new Set(models.map((m) => m.vendor))].map((vendor) => ({ vendor, count: models.filter((m) => m.vendor === vendor).length }))
      json = { total: models.length, capabilities: [...new Set(models.flatMap((m) => Object.entries(m.capabilities).filter(([, value]) => value === true).map(([key]) => key)))], has_context: models.some((m) => m.context_window !== null), vendors, vendors_page: { total: vendors.length, limit: 100, offset: 0, has_more: false, next_offset: null }, pricing_epoch: 1 }
    }
    if (path === '/admin/models') {
      if (request.method() === 'POST') { posts.push(request.postDataJSON()); json = { model_id: 1, requires_publish: true } }
      else json = { data: models, total: models.length, unpriced: 0, base_price_per_1m_micro: base }
    }
    return route.fulfill({ json })
  })
  await page.goto('/admin/pricing')
  return posts
}

test('模型表单基础校验和精确价格，未声明不会伪装成不支持', () => {
  expect(zh.common).toBe(zhBase.common)
  expect(zh.logs).toBe(zhBase.logs)
  expect(zh.admin.pricingBase).toBe(zhBase.admin.pricingBase)
  expect(zh.admin.modelMeta.kind).toBe(zhBase.admin.modelMeta.kind)
  expect(metadataPayload(metadataDraft()).capabilities).toEqual({})
  expect(metadataPayload(metadataDraft()).context_window).toBeNull()
  expect(usdMicro('0.012345')).toBe(12345)
  expect(usdMicro(priceFromMicro(Number.MAX_SAFE_INTEGER))).toBe(Number.MAX_SAFE_INTEGER)
  expect(priceFromMicro(0)).toBe('0')
  for (const v of ['-1', '1e3', '0.0000001', '', 'NaN']) expect(usdMicro(v)).toBeNull()
  expect(validRatio('0')).toBe(true)
  expect(validRatio('0.123456')).toBe(true)
  expect(validRatio('0.1234567')).toBe(false)
  for (const v of ['0', '-1', '1.5', '2147483648']) expect(validLimit(v)).toBe(false)
})

test('价格可编辑性区分输入、输出和缓存：未知不禁用，名称和类型不猜测能力', () => {
  const axes = [...TEXT_AXES, ...MODAL_AXES, ...INDEPENDENT_AXES]
  const unknown = metadataDraft()
  for (const axis of axes) expect(pricingDisabledReason(unknown, axis)).toBeUndefined()
  for (const kind of MODEL_KINDS) {
    for (const axis of axes) expect(pricingDisabledReason({ ...unknown, kind }, axis)).toBeUndefined()
  }
  const vision = presetMetadata(findModelPreset('gpt-6-astra')!)
  for (const axis of ['image_ratio', 'image_cache_read', 'image_cache_write', 'cache_write_1h'] as const) {
    expect(pricingDisabledReason(vision, axis)).toBeUndefined()
  }
  expect(pricingDisabledReason(vision, 'image_output')).toBe('admin:modelMeta.noImageOutput')
  for (const axis of ['audio_ratio', 'audio_cache_read', 'audio_cache_write'] as const) {
    expect(pricingDisabledReason(vision, axis)).toBe('admin:modelMeta.noAudioInput')
  }
  expect(pricingDisabledReason(vision, 'audio_completion_ratio')).toBe('admin:modelMeta.noAudioOutput')
  const image = presetMetadata(findModelPreset('gemini-3.1-flash-image')!)
  expect(pricingDisabledReason(image, 'image_output')).toBeUndefined()
  expect(pricingDisabledReason(image, 'image_ratio')).toBeUndefined()
  for (const axis of ['cache_ratio', 'cache_write_ratio', 'cache_write_5m', 'cache_write_1h', 'image_cache_read', 'audio_cache_read', 'image_cache_write', 'audio_cache_write'] as const) {
    expect(pricingDisabledReason(image, axis)).toBe('admin:modelMeta.noPromptCache')
  }
  const textOnly = { ...unknown, input_modalities: ['text'], output_modalities: ['text'], capabilities: { vision: true, audio: true } }
  expect(pricingDisabledReason(textOnly, 'image_ratio')).toBe('admin:modelMeta.noImageInput')
  expect(pricingDisabledReason(textOnly, 'audio_completion_ratio')).toBe('admin:modelMeta.noAudioOutput')
  // Vision describes image understanding, not image generation; an explicit negative still guards input.
  const generation = { ...unknown, input_modalities: ['image'], output_modalities: ['image'], capabilities: { vision: false } }
  expect(pricingDisabledReason(generation, 'image_ratio')).toBe('admin:modelMeta.noImageInput')
  expect(pricingDisabledReason(generation, 'image_output')).toBeUndefined()
  expect(pricingDisabledReason({ ...unknown, capabilities: { audio: false } }, 'audio_completion_ratio')).toBe('admin:modelMeta.noAudioOutput')
  for (const reason of ['noImageInput', 'noAudioInput', 'noImageOutput', 'noAudioOutput', 'noPromptCache', 'rateAvailabilityHint'] as const) {
    for (const locale of [zh, en]) expect(locale.admin.modelMeta[reason]).toBeTruthy()
  }
})

test('选择不同模型实时禁用不适用价格，图像生成不禁用输出价格，缓存否定约束所有缓存项', async ({ page }) => {
  const posts = await setup(page)
  await page.getByRole('button', { name: '新建模型', exact: true }).click()
  const d = page.getByRole('dialog')
  // 抽屉打开后会把焦点移到首个控件；等它落定再输入，否则焦点跳走会收起候选
  await expect(d.locator(':focus')).toHaveCount(1)
  await d.locator('#m-name').fill('gpt-6-astra')
  await d.getByRole('option').filter({ hasText: 'gpt-6-astra' }).click()
  await d.locator('#model-advanced-section > summary').click()
  await d.locator('#model-cache-section > summary').click()
  await d.locator('#model-modal-section > summary').click()
  await d.locator('#model-cache-ttl-section > summary').click()
  await d.locator('#model-modal-cache-section > summary').click()
  await d.locator('#model-unavailable-section > summary').click()
  for (const id of ['ax-audio_ratio', 'ax-audio_completion_ratio', 'ind-audio_cache_read', 'ind-audio_cache_write', 'ind-image_output']) {
    await expect(d.locator(`#${id}`)).toBeDisabled()
    await expect(d.locator(`#${id}`)).toHaveAttribute('aria-describedby', `${id}-reason`)
    await expect(d.locator(`#${id}-reason`)).toBeVisible()
  }
  for (const id of ['ax-image_ratio', 'ind-image_cache_read', 'ind-image_cache_write', 'ind-cache_write_1h']) await expect(d.locator(`#${id}`)).toBeEnabled()
  await d.locator('#ind-image_cache_read').fill('0.15')
  await d.locator('#model-cache-section > summary').click()
  await d.locator('#model-unavailable-section > summary').click()
  await d.locator('#model-modal-section').scrollIntoViewIfNeeded()
  await page.screenshot({ path: 'test-results/model-pricing-disabled.png', animations: 'disabled' })
  await d.locator('#m-name').fill('gemini-3.1-flash-image')
  await d.getByRole('option').filter({ hasText: 'gemini-3.1-flash-image' }).click()
  await expect(d.locator('#ind-image_output')).toBeEnabled()
  await d.locator('#ind-image_output').fill('3')
  await d.locator('#model-unavailable-section > summary').click()
  await expect(d.locator('#ind-image_cache_read')).toBeDisabled()
  await expect(d.locator('#ind-image_cache_read')).toHaveValue('0.15')
  await expect(d.locator('#ind-image_cache_read-reason')).toHaveText('当前声明不支持提示缓存')
  await expect(d.locator('#model-cache-section')).toHaveCount(0)
  for (const id of ['ax-cache_ratio', 'ax-cache_write_ratio', 'ind-cache_write_5m', 'ind-cache_write_1h']) await expect(d.locator(`#${id}`)).toBeDisabled()
  await d.locator('#m-name').fill('gemini-2.5-pro')
  await d.getByRole('option').filter({ hasText: 'gemini-2.5-pro' }).click()
  await expect(d.locator('#ax-audio_ratio')).toBeEnabled()
  await expect(d.locator('#ind-audio_cache_read')).toBeEnabled()
  await expect(d.locator('#ax-audio_completion_ratio')).toBeDisabled()
  await expect(d.locator('#ind-image_output')).toBeDisabled()
  await expect(d.locator('#ind-image_output')).toHaveValue('3')
  await expect(d.locator('#ind-image_cache_read')).toHaveValue('0.15')
  await d.getByRole('button', { name: '保存', exact: true }).click()
  await expect(d).toHaveCount(0)
  expect(posts[0]).toMatchObject({ model_name: 'gemini-2.5-pro', modality_ratios: { image_output: '3', image_cache_read: '0.15' } })
})

test('未声明自定义模型可以填写，修改模态和视觉标签立即联动，禁用再启用不丢草稿', async ({ page }) => {
  const posts = await setup(page)
  await page.getByRole('button', { name: '新建模型', exact: true }).click()
  const d = page.getByRole('dialog')
  // 抽屉打开后会把焦点移到首个控件；等它落定再输入，否则焦点跳走会收起候选
  await expect(d.locator(':focus')).toHaveCount(1)
  await d.locator('#m-name').fill('unknown-custom')
  await d.locator('#model-advanced-section > summary').click()
  await d.locator('#model-modal-section > summary').click()
  await d.locator('#model-modal-cache-section > summary').click()
  for (const axis of [...MODAL_AXES, ...INDEPENDENT_AXES.slice(2)]) {
    await expect(d.locator(`#${MODAL_AXES.includes(axis as typeof MODAL_AXES[number]) ? 'ax' : 'ind'}-${axis}`)).toBeEnabled()
  }
  await d.locator('#ind-audio_cache_read').fill('0.25')
  await d.locator('#ind-image_output').fill('2.5')
  await d.locator('#model-metadata-section > summary').click()
  await d.getByRole('checkbox', { name: '输入模态 · 文本' }).check()
  await d.getByRole('checkbox', { name: '输出模态 · 文本' }).check()
  await expect(d.locator('#ind-audio_cache_read')).toBeDisabled()
  await expect(d.locator('#ind-image_output')).toBeDisabled()
  await expect(d.locator('#ind-audio_cache_read')).toHaveValue('0.25')
  await d.getByRole('checkbox', { name: '输入模态 · 音频' }).check()
  await d.getByRole('checkbox', { name: '输入模态 · 图像' }).check()
  await d.getByRole('checkbox', { name: '输出模态 · 图像' }).check()
  await expect(d.locator('#ind-audio_cache_read')).toBeEnabled()
  await expect(d.locator('#ind-image_output')).toBeEnabled()
  await expect(d.locator('#ind-image_output')).toHaveValue('2.5')
  await d.locator('#model-capability-editor > summary').click()
  await d.locator('#cap-vision').selectOption('false')
  await expect(d.locator('#ax-image_ratio')).toBeDisabled()
  await expect(d.locator('#ind-image_cache_read')).toBeDisabled()
  await expect(d.locator('#ind-image_output')).toBeEnabled()
  await d.locator('#cap-vision').selectOption('true')
  await expect(d.locator('#ax-image_ratio')).toBeEnabled()
  await expect(d.locator('#ind-image_cache_read')).toBeEnabled()
  await d.getByRole('button', { name: '保存', exact: true }).click()
  await expect(d).toHaveCount(0)
  expect(posts[0]).toMatchObject({ metadata: { input_modalities: ['text', 'audio', 'image'], output_modalities: ['text', 'image'], capabilities: { vision: true } },
    modality_ratios: { audio_cache_read: '0.25', image_output: '2.5' } })
})

test('历史不适用倍率灰显保留，编辑并保存不会清空缓存倍率、基础价及其他配置', async ({ page }) => {
  const source = { ...existing, pricing_mode: 'ratio', capabilities: { vision: true, prompt_cache: false },
    modality_ratios: { ...existing.modality_ratios, image_output: '2.5' }, fallback_models: ['spare'] }
  const posts = await setup(page, [source])
  await page.getByRole('row').filter({ hasText: source.model_name }).getByRole('button', { name: '编辑', exact: true }).click()
  const d = page.getByRole('dialog')
  await d.locator('#model-advanced-section > summary').click()
  await expect(d.locator('#model-cache-section')).toHaveCount(0)
  await d.locator('#model-modal-section > summary').click()
  await d.locator('#model-unavailable-section > summary').click()
  for (const axis of INDEPENDENT_AXES) await expect(d.locator(`#ind-${axis}`)).toBeDisabled()
  await expect(d.locator('#ax-cache_ratio')).toBeDisabled()
  await expect(d.locator('#ax-cache_ratio')).toHaveValue('0.1')
  await expect(d.locator('#ind-audio_cache_read')).toHaveValue('0.3')
  await expect(d.locator('#ind-image_output')).toHaveValue('2.5')
  await expect(d.getByRole('button', { name: '保存', exact: true })).toBeEnabled()
  await d.getByRole('button', { name: '保存', exact: true }).click()
  await expect(d).toHaveCount(0)
  expect(posts[0]).toMatchObject({ model_ratio: source.model_ratio, completion_ratio: source.completion_ratio, cache_ratio: source.cache_ratio,
    cache_write_ratio: source.cache_write_ratio, modality_ratios: source.modality_ratios, tier_ratios: source.tier_ratios,
    metadata: { capabilities: source.capabilities }, fallback_models: source.fallback_models })
})

test('官方预设数据白名单、来源、独立副本和促销期限；不按名称前缀猜测', () => {
  expect(MODEL_PRESETS).toHaveLength(32)
  expect(new Set(MODEL_PRESETS.map((p) => p.id)).size).toBe(MODEL_PRESETS.length)
  expect(new Set(MODEL_PRESETS.map((p) => p.vendor)).size).toBe(6)
  const sourceHosts = ['developers.openai.com', 'platform.claude.com', 'ai.google.dev', 'api-docs.deepseek.com', 'www.alibabacloud.com', 'platform.kimi.ai']
  for (const preset of MODEL_PRESETS) {
    expect(sourceHosts).toContain(new URL(preset.source).hostname)
    expect(new URL(preset.source).protocol).toBe('https:')
    expect(preset.checkedAt).toBe('2026-09-30')
    expect(MODEL_KINDS).toContain(preset.kind)
    for (const locale of [zh, en]) {
      expect(locale.admin.modelKinds[preset.kind]).toBeTruthy()
      for (const notice of preset.notices ?? []) expect(locale.admin.modelPresetNotices[notice]).toBeTruthy()
    }
    const metadata = presetMetadata(preset)
    expect(validLimit(metadata.context_window)).toBe(true)
    expect(validLimit(metadata.max_output)).toBe(true)
    for (const [key, supported] of Object.entries(preset.capabilities)) {
      expect(CAPABILITY_KEYS).toContain(key)
      expect(typeof supported).toBe('boolean')
    }
    const cache = presetCache(preset)
    expect(validRatio(cache.cache_ratio)).toBe(true)
    expect(validRatio(cache.cache_write_ratio)).toBe(true)
    for (const ratio of Object.values(cache.ttl)) expect(validRatio(ratio)).toBe(true)
    if (preset.referencePrice) expect(ratiosFromPrices(2_000_000, preset.referencePrice.input, preset.referencePrice.output)).not.toBeNull()
  }
  expect(findModelPreset('gpt-6-astra-custom')).toBeUndefined()
  expect(findModelPreset('kimi-k2.5')).toBeUndefined()
  const flash = findModelPreset('gemini-3.8-flash')!
  expect(referencePriceAvailable(flash, '2026-12-31')).toBe(true)
  expect(referencePriceAvailable(flash, '2027-01-01')).toBe(false)
  const draft = presetMetadata(findModelPreset('gpt-6-astra')!)
  draft.capabilities.vision = false; draft.input_modalities.push('audio')
  expect(presetMetadata(findModelPreset('gpt-6-astra')!).capabilities.vision).toBe(true)
  expect(presetMetadata(findModelPreset('gpt-6-astra')!).input_modalities).toEqual(['text', 'image'])
  expect(presetCache(findModelPreset('claude-opus-5-5')!)).toMatchObject({ cache_ratio: '0.05', ttl: { cache_write_5m: '1.25', cache_write_1h: '2' } })
  expect(presetMetadata(findModelPreset('qwen3.6-plus')!).capabilities.prompt_cache).toBe(true)
  expect(presetMetadata(findModelPreset('qwen3.6-plus-2026-04-02')!).capabilities.prompt_cache).toBe(false)
  expect(presetMetadata(findModelPreset('kimi-k3')!).max_output).toBe('')
  expect(presetCache(findModelPreset('kimi-k3')!).ttl).toEqual({})
  expect(presetMetadata(findModelPreset('deepseek-flash')!).max_output).toBe('393216')
})

test('选择官方模型带入规格但不改价格；参考价需点击，不发送保存和发布请求', async ({ page }) => {
  const posts = await setup(page, [], 3_000_000)
  await page.getByRole('button', { name: '新建模型', exact: true }).click()
  const d = page.getByRole('dialog')
  // 抽屉打开后会把焦点移到首个控件；等它落定再输入，否则焦点跳走会收起候选
  await expect(d.locator(':focus')).toHaveCount(1)
  await d.locator('#price-input').fill('4.5'); await d.locator('#price-output').fill('18')
  await d.locator('#m-name').fill('gpt-6-astra')
  await expect(d.getByTestId('model-preset-summary')).toHaveCount(0)
  await d.getByRole('option').filter({ hasText: 'gpt-6-astra' }).click()
  await expect(d.getByTestId('model-preset-summary')).toContainText('已带入预设规格')
  await expect(d.locator('#price-input')).toHaveValue('4.5')
  await expect(d.locator('#price-output')).toHaveValue('18')
  await expect(d.locator('#meta-vendor')).not.toBeVisible()
  await expect(d.getByRole('link', { name: '官方规格 · 核对 2026-09-30' })).toHaveAttribute('href', 'https://developers.openai.com/api/docs/models/gpt-6-astra')
  expect(posts).toHaveLength(0)
  await page.screenshot({ path: 'test-results/model-preset-default.png', animations: 'disabled' })
  await d.getByRole('button', { name: '填入参考价' }).click()
  await expect(d.locator('#price-input')).toHaveValue('10')
  await expect(d.locator('#price-output')).toHaveValue('50')
  await expect(d.getByText(/超过 272K 输入/)).toBeVisible()
  expect(posts).toHaveLength(0)
  await d.getByRole('button', { name: '保存', exact: true }).click()
  await expect(d).toHaveCount(0)
  expect(posts[0]).toMatchObject({ model_name: 'gpt-6-astra', model_ratio: '3.333333', completion_ratio: '5', cache_ratio: '0.1', cache_write_ratio: '1.25',
    metadata: { vendor: 'OpenAI', kind: 'chat', context_window: 1050000, max_output: 128000, input_modalities: ['text', 'image'],
      capabilities: { vision: true, tools: true, reasoning: true } } })
  expect(posts).toHaveLength(1)
})

test('预设按供应商筛选，数量来自目录；搜索仅限当前供应商，空结果支持自定义 ID', async ({ page }) => {
  const posts = await setup(page)
  await page.getByRole('button', { name: '新建模型', exact: true }).click()
  const d = page.getByRole('dialog'), vendor = d.locator('#m-preset-vendor')
  await expect(vendor).toHaveValue('')
  await expect(vendor.locator('option')).toHaveCount(7)
  await expect(d.getByText(/当前范围 32 个预设/)).toBeVisible()
  const list = d.getByRole('listbox').getByRole('option')
  for (const supplier of new Set(MODEL_PRESETS.map((preset) => preset.vendor))) {
    await vendor.selectOption(supplier)
    await d.locator('#m-name').click()
    const presets = MODEL_PRESETS.filter((preset) => preset.vendor === supplier)
    // 官方快照 ID（如带日期的 Claude ID）紧跟在其别名后面，也可直接选
    const ids = presets.flatMap((preset) => [preset.id, ...(preset.aliases ?? [])])
    await expect(list).toHaveCount(ids.length)
    await expect(list.locator('> span:first-child')).toHaveText(ids)
    await expect(d.getByText(new RegExp(`当前范围 ${presets.length} 个预设`))).toBeVisible()
  }
  await vendor.selectOption('OpenAI')
  await d.locator('#m-name').fill('gpt-4.1')
  await expect(list).toHaveCount(2)
  await d.locator('#m-name').fill('claude')
  await expect(list).toHaveCount(0)
  await expect(d.getByText('没有匹配预设，可保留此 ID 并手动配置。')).toBeVisible()
  await d.locator('#m-name').fill('private-openai')
  await d.locator('#price-input').fill('2')
  await d.getByRole('button', { name: '保存', exact: true }).click()
  await expect(d).toHaveCount(0)
  expect(posts[0]).toMatchObject({ model_name: 'private-openai', metadata: { vendor: null, capabilities: {} } })
  expect(posts[0]).not.toHaveProperty('presetVendor')
})

test('切换供应商只浏览候选，保留已选模型规格及价格；可再次选择或用键盘浏览', async ({ page }) => {
  const posts = await setup(page)
  await page.getByRole('button', { name: '新建模型', exact: true }).click()
  const d = page.getByRole('dialog'), vendor = d.locator('#m-preset-vendor')
  await d.locator('#price-input').fill('4.5'); await d.locator('#price-output').fill('18')
  await vendor.selectOption('Anthropic')
  await d.locator('#m-name').click()
  await d.getByRole('listbox').getByRole('option').filter({ hasText: 'claude-sonnet-5-5' }).click()
  await vendor.selectOption('Google')
  await expect(d.locator('#m-name')).toHaveValue('claude-sonnet-5-5')
  await expect(d.getByTestId('model-preset-summary')).toContainText('Anthropic')
  await expect(d.locator('#price-input')).toHaveValue('4.5')
  await expect(d.locator('#price-output')).toHaveValue('18')
  expect(posts).toHaveLength(0)
  await d.locator('#m-name').click()
  await expect(d.getByRole('listbox').getByRole('option')).toHaveCount(5)
  await expect(d.getByRole('listbox')).not.toContainText('claude-')
  await page.screenshot({ path: 'test-results/model-preset-vendor.png', animations: 'disabled' })
  await d.getByRole('listbox').getByRole('option').filter({ hasText: 'gemini-2.5-pro' }).click()
  await expect(d.locator('#m-name')).toHaveValue('gemini-2.5-pro')
  await expect(d.getByTestId('model-preset-summary')).toContainText('Google')
  await expect(d.locator('#price-input')).toHaveValue('4.5')
  await expect(d.locator('#price-output')).toHaveValue('18')
  await vendor.selectOption('Moonshot')
  await d.locator('#m-name').focus()
  await d.locator('#m-name').press('ArrowDown')
  await d.locator('#m-name').press('Enter')
  await expect(d.locator('#m-name')).toHaveValue('kimi-k3')
  await vendor.selectOption('')
  await d.locator('#m-name').click()
  await expect(d.getByRole('listbox').getByRole('option')).toHaveCount(
    MODEL_PRESETS.reduce((n, preset) => n + 1 + (preset.aliases?.length ?? 0), 0))
  await d.locator('#m-name').press('Escape')
  await expect(d).toBeVisible()
  await d.getByRole('button', { name: '保存', exact: true }).click()
  await expect(d).toHaveCount(0)
  expect(posts[0]).toMatchObject({ model_name: 'kimi-k3', metadata: { vendor: 'Moonshot' }, model_ratio: '2.25', completion_ratio: '4' })
  expect(posts).toHaveLength(1)
})

test('切换模型清理上一预设缓存时长和不兼容能力；自定义不继承官方规格', async ({ page }) => {
  const posts = await setup(page)
  await page.getByRole('button', { name: '新建模型', exact: true }).click()
  const d = page.getByRole('dialog')
  // 抽屉打开后会把焦点移到首个控件；等它落定再输入，否则焦点跳走会收起候选
  await expect(d.locator(':focus')).toHaveCount(1)
  await d.locator('#m-name').fill('claude-sonnet-5-5')
  await d.getByRole('option').filter({ hasText: 'claude-sonnet-5-5' }).click()
  await expect(d.getByTestId('model-preset-summary')).toContainText('5m / 1h')
  await d.locator('#m-name').fill('deepseek-v4-pro')
  await d.getByRole('option').filter({ hasText: 'deepseek-v4-pro' }).click()
  await d.locator('#model-metadata-section > summary').click()
  await expect(d.locator('#cap-vision')).toHaveValue('false')
  await expect(d.locator('#cap-structured_output')).toHaveValue('')
  await d.locator('#model-advanced-section > summary').click()
  await d.locator('#model-cache-section > summary').click()
  await expect(d.locator('#ind-cache_write_1h')).toHaveValue('')
  await expect(d.locator('#ax-cache_ratio')).toHaveValue('0.033333')
  await d.locator('#m-name').fill('private-deployment')
  await expect(d.getByTestId('model-preset-summary')).toHaveCount(0)
  await expect(d.locator('#meta-vendor')).toHaveValue('')
  await expect(d.locator('#cap-vision')).toHaveValue('')
  await expect(d.locator('#ax-cache_ratio')).toHaveValue('1')
  await d.getByRole('button', { name: '保存', exact: true }).click()
  await expect(d).toHaveCount(0)
  expect(posts[0]).toMatchObject({ model_name: 'private-deployment', metadata: { capabilities: {}, context_window: null }, modality_ratios: {} })
})

test('编辑已知模型仅展示预设参考，保存不覆盖原能力价格；替换规格必须明确点击', async ({ page }) => {
  const source = { ...existing, model_name: 'gpt-6-astra', pricing_mode: 'ratio', fallback_models: ['spare'] }
  const posts = await setup(page, [source])
  await page.getByRole('row').filter({ hasText: source.model_name }).getByRole('button', { name: '编辑', exact: true }).click()
  const d = page.getByRole('dialog')
  await expect(d.getByTestId('model-preset-summary')).toContainText('不自动覆盖现有配置')
  await expect(d.locator('#m-name')).toHaveAttribute('readonly', '')
  await expect(d.locator('#m-preset-vendor')).toHaveCount(0)
  await d.getByRole('button', { name: '保存', exact: true }).click()
  await expect(d).toHaveCount(0)
  expect(posts[0]).toMatchObject({ model_ratio: source.model_ratio, completion_ratio: source.completion_ratio, cache_ratio: source.cache_ratio,
    modality_ratios: source.modality_ratios, tier_ratios: source.tier_ratios, fallback_models: source.fallback_models,
    metadata: { capabilities: source.capabilities, context_window: source.context_window } })
  await page.getByRole('row').filter({ hasText: source.model_name }).getByRole('button', { name: '编辑', exact: true }).click()
  await d.locator('#model-metadata-section > summary').click()
  await d.getByRole('button', { name: '用预设替换规格' }).click()
  await expect(d.locator('#cap-tools')).toHaveValue('true')
  await expect(d.locator('#meta-description')).toHaveValue('人工声明')
  await d.getByRole('button', { name: '保存', exact: true }).click()
  await expect(d).toHaveCount(0)
  expect(posts[1]).toMatchObject({ cache_ratio: source.cache_ratio, modality_ratios: source.modality_ratios, tier_ratios: source.tier_ratios,
    metadata: { capabilities: { tools: true }, context_window: 1050000, display_name: '视觉助手', description: '人工声明' } })
})

test('图像与未核实计价预设不能填通用参考价，未知输出上限保持空白', async ({ page }) => {
  const posts = await setup(page)
  await page.getByRole('button', { name: '新建模型', exact: true }).click()
  const d = page.getByRole('dialog')
  // 抽屉打开后会把焦点移到首个控件；等它落定再输入，否则焦点跳走会收起候选
  await expect(d.locator(':focus')).toHaveCount(1)
  await d.locator('#m-name').fill('gemini-3.1-flash-image')
  await d.getByRole('option').filter({ hasText: 'gemini-3.1-flash-image' }).click()
  await expect(d.getByRole('button', { name: '填入参考价' })).toHaveCount(0)
  await d.locator('#model-metadata-section > summary').click()
  await expect(d.locator('#meta-kind')).toHaveValue('image_generation')
  await expect(d.locator('#cap-tools')).toHaveValue('false')
  await expect(d.locator('#cap-tools')).not.toBeVisible()
  await d.locator('#model-capability-editor > summary').click()
  await d.locator('#cap-tools').selectOption('true')
  await expect(d.getByTestId('model-preset-summary').getByText('工具调用', { exact: true })).toBeVisible()
  await d.locator('#cap-tools').selectOption('false')
  await expect(d.getByTestId('model-preset-summary').getByText('工具调用', { exact: true })).toHaveCount(0)
  await expect(d.getByRole('checkbox', { name: '输出模态 · 图像' })).toBeChecked()
  await d.locator('#m-name').fill('kimi-k3')
  await d.getByRole('option').filter({ hasText: 'kimi-k3' }).click()
  await expect(d.locator('#meta-max_output')).toHaveValue('')
  await d.getByRole('button', { name: '保存', exact: true }).click()
  await expect(d).toHaveCount(0)
  expect(posts[0]).toMatchObject({ model_name: 'kimi-k3', metadata: { max_output: null }, modality_ratios: {} })
})

test('单价和倍率精确转换，基准价、零价、超限和舍入不会混淆', () => {
  expect(pricesFromRatios(3_000_000, '1.5', '4')).toEqual({ input: '4.5', output: '18' })
  expect(ratiosFromPrices(3_000_000, '4.5', '18')).toMatchObject({ model_ratio: '1.5', completion_ratio: '4', approximate: false })
  expect(ratiosFromPrices(2_000_000, '0.15', '0.6')).toMatchObject({ model_ratio: '0.075', completion_ratio: '4' })
  expect(ratiosFromPrices(2_000_000, '0', '0')).toMatchObject({ model_ratio: '0', approximate: false })
  expect(ratiosFromPrices(2_000_000, '0', '1')).toBeNull()
  expect(ratiosFromPrices(2_000_000, '0.000000001', '0')).toBeNull()
  expect(ratiosFromPrices(2_000_000, '1', '0.0000000001')).toBeNull()
  expect(ratiosFromPrices(2_000_000, '3000000', '0')).toBeNull()
  expect(ratiosFromPrices(2_000_000, '3', '10')).toMatchObject({ completion_ratio: '3.333333', approximate: true, effective: { input: '3', output: '9.999999' } })
  for (const raw of ['', '-1', 'NaN', '1e2', '1.1234567890123456789']) expect(ratiosFromPrices(2_000_000, raw, '1')).toBeNull()
  // Exact existing six-place rates round-trip even when their USD price has 18 decimals.
  const exact = pricesFromRatios(1, '0.000001', '0.000001')
  expect(exact).toEqual({ input: '0.000000000001', output: '0.000000000000000001' })
  expect(ratiosFromPrices(1, exact.input, exact.output)).toMatchObject({ model_ratio: '0.000001', completion_ratio: '0.000001', approximate: false })
})

test('新建模型：类型、独立输入输出、常用能力标签、限制和时长倍率可配置', async ({ page }) => {
  const posts = await setup(page)
  await page.getByRole('button', { name: '新建模型', exact: true }).click()
  const d = page.getByRole('dialog')
  // 抽屉打开后会把焦点移到首个控件；等它落定再输入，否则焦点跳走会收起候选
  await expect(d.locator(':focus')).toHaveCount(1)
  await d.locator('#m-name').fill('new-multimodal')
  await d.locator('#model-metadata-section > summary').click()
  await d.getByLabel('显示名称', { exact: true }).fill('多模态助手')
  await d.getByLabel('模型类型', { exact: true }).selectOption('chat')
  await d.getByRole('checkbox', { name: '输入模态 · 文本' }).check()
  await d.getByRole('checkbox', { name: '输入模态 · 图像' }).check()
  await d.getByRole('checkbox', { name: '输出模态 · 文本' }).check()
  await d.getByRole('checkbox', { name: '输出模态 · 图像' }).check()
  await page.screenshot({ path: 'test-results/model-config-info.png' })
  await expect(d.getByLabel('工具调用', { exact: true })).toHaveValue('')
  await expect(d.getByLabel('工具调用', { exact: true })).not.toBeVisible()
  await d.locator('#model-capability-editor > summary').click()
  await d.getByLabel('视觉理解').selectOption('true')
  await d.getByLabel('工具调用', { exact: true }).selectOption('false')
  await d.locator('#model-limits-section > summary').click()
  await d.getByLabel('上下文窗口（Token）').fill('0')
  await expect(d.getByRole('button', { name: '保存', exact: true })).toBeDisabled()
  await d.getByLabel('上下文窗口（Token）').fill('128000')
  await d.getByLabel('最大输出（Token）').fill('8192')
  await page.screenshot({ path: 'test-results/model-config-capabilities.png' })
  await d.locator('#model-metadata-section > summary').click()
  await d.locator('#model-advanced-section > summary').click()
  await d.locator('#model-cache-section > summary').click()
  await d.locator('#model-cache-ttl-section > summary').click()
  await d.locator('#ind-cache_write_5m').fill('1.25')
  await d.locator('#ind-cache_write_1h').fill('2')
  await d.locator('#model-modal-section > summary').click()
  await d.locator('#ind-image_output').fill('0')
  await d.getByRole('button', { name: '保存', exact: true }).click()
  await expect(d).toHaveCount(0)
  expect(posts[0]).toMatchObject({ pricing_mode: 'ratio', metadata: {
    display_name: '多模态助手', kind: 'chat', context_window: 128000, max_output: 8192,
    input_modalities: ['text', 'image'], output_modalities: ['text', 'image'], capabilities: { vision: true, tools: false },
  }, modality_ratios: { cache_write_5m: '1.25', cache_write_1h: '2', image_output: '0' } })
  expect((posts[0].metadata as { capabilities: object }).capabilities).not.toHaveProperty('reasoning')
})

test('能力配置由 16 个开关精简为 5 个常用标签；默认折叠，未声明不写成不支持', async ({ page }) => {
  const posts = await setup(page)
  await page.getByRole('button', { name: '新建模型', exact: true }).click()
  const d = page.getByRole('dialog')
  // 抽屉打开后会把焦点移到首个控件；等它落定再输入，否则焦点跳走会收起候选
  await expect(d.locator(':focus')).toHaveCount(1)
  await d.locator('#m-name').fill('label-only-model')
  await d.locator('#model-metadata-section > summary').click()
  await expect(d.getByTestId('model-capability-summary')).toContainText('未声明常用能力')
  await expect(d.locator('#model-capability-editor')).not.toHaveAttribute('open', '')
  const editable = ['vision', 'tools', 'reasoning', 'json', 'structured_output']
  for (const key of CAPABILITY_KEYS.filter((key) => !editable.includes(key))) await expect(d.locator(`#cap-${key}`)).toHaveCount(0)
  await d.locator('#model-capability-editor > summary').click()
  await expect(d.locator('#model-capability-editor select:visible')).toHaveCount(5)
  await d.locator('#cap-vision').selectOption('true')
  await d.locator('#cap-tools').selectOption('false')
  await expect(d.getByTestId('model-capability-summary')).toContainText('工具调用 · 不支持')
  await d.locator('#cap-tools').selectOption('')
  await expect(d.getByTestId('model-capability-summary')).not.toContainText('工具调用')
  await d.locator('#model-capability-editor > summary').click()
  await d.locator('#model-capability-editor').scrollIntoViewIfNeeded()
  await page.screenshot({ path: 'test-results/model-capability-tags.png', animations: 'disabled' })
  await d.getByRole('button', { name: '保存', exact: true }).click()
  await expect(d).toHaveCount(0)
  expect((posts[0].metadata as { capabilities: object }).capabilities).toEqual({ vision: true })
})

test('移除多余能力编辑项不清空历史声明、模态、缓存价或路由；修改只影响所选标签', async ({ page }) => {
  const caps = Object.fromEntries(CAPABILITY_KEYS.map((key, index) => [key, index % 2 === 0]))
  const source = { ...existing, capabilities: caps, pricing_mode: 'ratio', fallback_models: ['spare'] }
  const posts = await setup(page, [source])
  await page.getByRole('row').filter({ hasText: source.model_name }).getByRole('button', { name: '编辑', exact: true }).click()
  const d = page.getByRole('dialog')
  await d.locator('#model-metadata-section > summary').click()
  await expect(d.getByText('其他预设或历史能力声明保留，不需逐项配置。')).toBeVisible()
  await expect(d.locator('#cap-web_search')).toHaveCount(0)
  await expect(d.locator('#cap-prompt_cache')).toHaveCount(0)
  await d.getByRole('button', { name: '保存', exact: true }).click()
  await expect(d).toHaveCount(0)
  expect(posts[0]).toMatchObject({ metadata: { capabilities: caps, input_modalities: source.catalog_config.input_modalities },
    cache_ratio: source.cache_ratio, modality_ratios: source.modality_ratios, tier_ratios: source.tier_ratios, fallback_models: source.fallback_models })
  await page.getByRole('row').filter({ hasText: source.model_name }).getByRole('button', { name: '编辑', exact: true }).click()
  await d.locator('#model-metadata-section > summary').click()
  await d.locator('#model-capability-editor > summary').click()
  await d.locator('#cap-tools').selectOption('true')
  await d.locator('#cap-json').selectOption('')
  await d.getByRole('button', { name: '保存', exact: true }).click()
  await expect(d).toHaveCount(0)
  const { json: _json, ...retained } = caps
  expect((posts[1].metadata as { capabilities: object }).capabilities).toEqual({ ...retained, tools: true })
  expect(posts[1]).toMatchObject({ cache_ratio: source.cache_ratio, modality_ratios: source.modality_ratios,
    tier_ratios: source.tier_ratios, fallback_models: source.fallback_models })
})

test('编辑保留按次模式及既有档位，清空适用倍率是显式操作，不适用的历史值保留', async ({ page }) => {
  const posts = await setup(page, [existing])
  await page.getByRole('row').filter({ hasText: 'custom-vision' }).getByRole('button', { name: '编辑', exact: true }).click()
  const d = page.getByRole('dialog')
  await expect(d.locator('#m-pricing-mode')).toHaveValue('per_call')
  await expect(d.locator('#m-call-price')).toHaveValue('0.012345')
  await expect(d.locator('#tier-0')).not.toBeVisible()
  await expect(d.locator('#model-advanced-section')).not.toHaveAttribute('open', '')
  await d.getByRole('button', { name: '保存', exact: true }).click()
  await expect(d).toHaveCount(0)
  expect(posts[0]).toMatchObject({ pricing_mode: 'per_call', per_call_price_micro: 12345,
    tier_ratios: { flex: '0.5', priority: '2' }, modality_ratios: existing.modality_ratios, metadata: { kind: 'chat', max_output: 8192 } })
  await page.getByRole('row').filter({ hasText: 'custom-vision' }).getByRole('button', { name: '编辑', exact: true }).click()
  await d.locator('#m-pricing-mode').selectOption('ratio')
  await d.locator('#model-advanced-section > summary').click()
  await d.locator('#model-cache-section > summary').click()
  await d.locator('#model-modal-section > summary').click()
  await d.locator('#model-tiers-section > summary').click()
  await d.locator('#model-cache-ttl-section > summary').click()
  for (const key of ['cache_write_5m', 'cache_write_1h']) await d.locator(`#ind-${key}`).fill('')
  await expect(d.locator('#ind-audio_cache_read')).toBeDisabled()
  await expect(d.locator('#ind-audio_cache_read')).toHaveValue('0.3')
  await d.getByRole('button', { name: '删除', exact: true }).last().click()
  await d.getByRole('button', { name: '删除', exact: true }).last().click()
  await d.getByRole('button', { name: '保存', exact: true }).click()
  await expect(d).toHaveCount(0)
  expect(posts[1]).toMatchObject({ pricing_mode: 'ratio', tier_ratios: {}, modality_ratios: { audio_cache_read: '0.3' } })
})

test('默认精简表单：基础单价直接填写，高级折叠，按非默认基准价提交倍率', async ({ page }) => {
  const posts = await setup(page, [], 3_000_000)
  await page.getByRole('button', { name: '新建模型', exact: true }).click()
  const d = page.getByRole('dialog')
  // 抽屉打开后会把焦点移到首个控件；等它落定再输入，否则焦点跳走会收起候选
  await expect(d.locator(':focus')).toHaveCount(1)
  await expect(d.locator('#price-input')).toHaveValue('3')
  await expect(d.locator('#price-output')).toHaveValue('3')
  await expect(d.locator('#meta-kind')).not.toBeVisible()
  await expect(d.locator('#meta-vendor')).not.toBeVisible()
  await expect(d.locator('#m-price-editor')).toHaveCount(0)
  await expect(d.locator('#model-ratio-editor')).not.toHaveAttribute('open', '')
  for (const id of ['cap-tools', 'ind-cache_write_1h', 'ax-audio_ratio', 'm-fallbacks']) await expect(d.locator(`#${id}`)).not.toBeVisible()
  const visibleInputs = await d.locator('input:visible, select:visible').count()
  expect(visibleInputs).toBe(5)
  await d.locator('#m-name').fill('simple-model')
  await d.locator('#price-input').fill('4.5')
  await d.locator('#price-output').fill('18')
  await expect(d.getByRole('button', { name: '保存', exact: true })).toBeEnabled()
  await page.screenshot({ path: 'test-results/model-simple-default.png', animations: 'disabled' })
  await d.getByRole('button', { name: '保存', exact: true }).click()
  await expect(d).toHaveCount(0)
  expect(posts[0]).toMatchObject({ model_ratio: '1.5', completion_ratio: '4', metadata: { capabilities: {} }, tier_ratios: {}, modality_ratios: {} })
})

test('文本模型首屏仅五个常用控件，不适用多模态不展开，缓存默认只填通用读写', async ({ page }) => {
  const posts = await setup(page)
  await page.getByRole('button', { name: '新建模型', exact: true }).click()
  const d = page.getByRole('dialog')
  // 抽屉打开后会把焦点移到首个控件；等它落定再输入，否则焦点跳走会收起候选
  await expect(d.locator(':focus')).toHaveCount(1)
  await d.locator('#m-name').fill('deepseek-v4-pro')
  await d.getByRole('option').filter({ hasText: 'deepseek-v4-pro' }).click()
  await expect(d.locator('input:visible, select:visible')).toHaveCount(5)
  for (const id of ['model-ratio-editor', 'model-metadata-section', 'model-advanced-section']) {
    await expect(d.locator(`#${id}`)).not.toHaveAttribute('open', '')
  }
  for (const id of ['meta-vendor', 'meta-kind', 'meta-context_window', 'meta-max_output']) await expect(d.locator(`#${id}`)).not.toBeVisible()
  const advancedSummary = await d.locator('#model-advanced-section > summary').boundingBox()
  const footer = await d.locator('footer').boundingBox()
  expect(advancedSummary!.y + advancedSummary!.height).toBeLessThanOrEqual(footer!.y)
  await page.screenshot({ path: 'test-results/model-light-pricing.png', animations: 'disabled' })
  await d.locator('#model-advanced-section > summary').click()
  await expect(d.locator('#model-advanced-section > summary > svg')).toHaveCSS('rotate', '180deg')
  await expect(d.locator('#model-cache-section > summary > svg')).toHaveCSS('rotate', 'none')
  await expect(d.locator('#model-modal-section')).toHaveCount(0)
  await expect(d.locator('#model-unavailable-section')).not.toHaveAttribute('open', '')
  await expect(d.locator('#model-unavailable-section input:visible')).toHaveCount(0)
  await d.locator('#model-cache-section > summary').click()
  await expect(d.locator('#model-cache-section > summary > svg')).toHaveCSS('rotate', '180deg')
  await expect(d.locator('#model-cache-ttl-section > summary > svg')).toHaveCSS('rotate', 'none')
  await expect(d.locator('#model-cache-section input:visible')).toHaveCount(2)
  await expect(d.locator('#model-cache-ttl-section')).not.toHaveAttribute('open', '')
  await expect(d.locator('#model-advanced-section input:disabled:visible')).toHaveCount(0)
  await d.locator('#model-cache-ttl-section > summary').click()
  await d.locator('#ind-cache_write_1h').fill('2')
  await d.getByRole('button', { name: '保存', exact: true }).click()
  await expect(d).toHaveCount(0)
  expect(posts[0]).toMatchObject({ model_name: 'deepseek-v4-pro', modality_ratios: { cache_write_1h: '2' }, metadata: { capabilities: { vision: false } } })
})

test('图像模型只展示适用图像价格，不出现音频或缓存输入，核对旧项仍禁止输入', async ({ page }) => {
  const posts = await setup(page)
  await page.getByRole('button', { name: '新建模型', exact: true }).click()
  const d = page.getByRole('dialog')
  // 抽屉打开后会把焦点移到首个控件；等它落定再输入，否则焦点跳走会收起候选
  await expect(d.locator(':focus')).toHaveCount(1)
  await d.locator('#m-name').fill('gemini-3.1-flash-image')
  await d.getByRole('option').filter({ hasText: 'gemini-3.1-flash-image' }).click()
  await d.locator('#model-advanced-section > summary').click()
  await d.locator('#model-modal-section > summary').click()
  await expect(d.locator('#model-modal-section input:visible')).toHaveCount(2)
  await expect(d.locator('#ax-image_ratio')).toBeVisible()
  await expect(d.locator('#ind-image_output')).toBeVisible()
  await expect(d.locator('#model-cache-section')).toHaveCount(0)
  await expect(d.locator('#model-modal-cache-section')).toHaveCount(0)
  await expect(d.locator('#ax-audio_ratio')).not.toBeVisible()
  await expect(d.locator('#ind-image_cache_read')).not.toBeVisible()
  await expect(d.locator('#model-advanced-section input:disabled:visible')).toHaveCount(0)
  await d.locator('#ind-image_output').fill('2.5')
  await d.locator('#model-modal-section').scrollIntoViewIfNeeded()
  await page.screenshot({ path: 'test-results/model-light-image.png', animations: 'disabled' })
  await d.locator('#model-unavailable-section > summary').click()
  await expect(d.locator('#ax-audio_ratio')).toBeVisible()
  await expect(d.locator('#ax-audio_ratio')).toBeDisabled()
  await expect(d.locator('#ind-image_cache_read')).toBeDisabled()
  await d.getByRole('button', { name: '保存', exact: true }).click()
  await expect(d).toHaveCount(0)
  expect(posts[0]).toMatchObject({ model_name: 'gemini-3.1-flash-image', modality_ratios: { image_output: '2.5' } })
})

test('单价不合法禁止保存；舍入提示实际价格，切回倍率不重复换算', async ({ page }) => {
  const posts = await setup(page)
  await page.getByRole('button', { name: '新建模型', exact: true }).click()
  const d = page.getByRole('dialog')
  // 抽屉打开后会把焦点移到首个控件；等它落定再输入，否则焦点跳走会收起候选
  await expect(d.locator(':focus')).toHaveCount(1)
  await d.locator('#m-name').fill('rounding-model')
  await d.locator('#price-input').fill('0')
  await d.locator('#price-output').fill('1')
  await expect(d.getByRole('button', { name: '保存', exact: true })).toBeDisabled()
  await expect(d.getByRole('alert')).toContainText('输入免费')
  await d.locator('#price-input').fill('3')
  await d.locator('#price-output').fill('10')
  await expect(d.getByRole('status').filter({ hasText: '9.999999' })).toBeVisible()
  await d.locator('#model-advanced-section > summary').click()
  await d.locator('#model-ratio-editor > summary').click()
  await expect(d.locator('#ax-model_ratio')).toHaveValue('1.5')
  await expect(d.locator('#ax-completion_ratio')).toHaveValue('3.333333')
  await d.locator('#ax-completion_ratio').fill('4')
  await d.locator('#model-ratio-editor > summary').click()
  await expect(d.locator('#price-output')).toHaveValue('12')
  await d.getByRole('button', { name: '保存', exact: true }).click()
  await expect(d).toHaveCount(0)
  expect(posts[0]).toMatchObject({ model_ratio: '1.5', completion_ratio: '4' })
})

test('编辑仅改展示信息保留全部已有倍率和隐藏的高级配置', async ({ page }) => {
  const source = { ...existing, pricing_mode: 'ratio', model_ratio: '0.123456', completion_ratio: '0.654321', fallback_models: ['spare'] }
  const posts = await setup(page, [source])
  await page.getByRole('row').filter({ hasText: source.model_name }).getByRole('button', { name: '编辑', exact: true }).click()
  const d = page.getByRole('dialog')
  const prices = pricesFromRatios(2_000_000, source.model_ratio, source.completion_ratio)
  await expect(d.locator('#price-output')).toHaveValue(prices.output)
  await d.locator('#model-metadata-section > summary').click()
  await d.locator('#meta-display_name').fill('新的展示名称')
  await d.getByRole('button', { name: '保存', exact: true }).click()
  await expect(d).toHaveCount(0)
  expect(posts[0]).toMatchObject({ model_ratio: source.model_ratio, completion_ratio: source.completion_ratio,
    tier_ratios: source.tier_ratios, modality_ratios: source.modality_ratios, fallback_models: source.fallback_models })
})

test('已有模型模板：手输和选择都不自动应用，明确确认后保留目标模型名称', async ({ page }) => {
  const posts = await setup(page, [existing])
  await page.getByRole('button', { name: '新建模型', exact: true }).click()
  const d = page.getByRole('dialog')
  // 抽屉打开后会把焦点移到首个控件；等它落定再输入，否则焦点跳走会收起候选
  await expect(d.locator(':focus')).toHaveCount(1)
  await d.locator('#m-name').fill('my-model')
  await d.locator('#model-metadata-section > summary').click()
  await d.locator('#model-template-section > summary').click()
  await d.locator('#model-template').fill('custom-vision')
  await expect(d.locator('#m-pricing-mode')).toHaveValue('ratio')
  await d.getByRole('option').filter({ hasText: 'custom-vision' }).click()
  await expect(d.locator('#m-pricing-mode')).toHaveValue('ratio')
  await d.getByRole('button', { name: '确认套用模板' }).click()
  await expect(d.locator('#m-pricing-mode')).toHaveValue('per_call')
  await expect(d.locator('#m-name')).toHaveValue('my-model')
  expect(posts).toHaveLength(0)
  await d.getByRole('button', { name: '保存', exact: true }).click()
  await expect(d).toHaveCount(0)
  expect(posts[0]).toMatchObject({ model_name: 'my-model', per_call_price_micro: 12345, metadata: { capabilities: existing.capabilities },
    modality_ratios: existing.modality_ratios, tier_ratios: existing.tier_ratios, fallback_models: [] })
})

test('目录独立单价和账单时长分项使用实际倍率而非叠乘通用倍率', () => {
  const model = { ...existing, model: existing.model_name, mode: 'ratio', base_price_per_1m_micro: 2000000, groups: [] } as PricingModel
  expect(modelPrice(model, 'cache_write_1h', 1)).toBe(4000000)
  expect(modelPrice(model, 'audio_cache_read', 1)).toBe(600000)
  const row = { usage_details_recorded: true, usage: {
    prompt_tokens: 1000, cached_tokens: 0, completion_tokens: 0, reasoning_tokens: 0,
    cache_write_tokens: 100, cache_write_5m_tokens: 60, cache_write_1h_tokens: 40,
    cache_write_reported: true, audio_prompt_tokens: 0, image_prompt_tokens: 0,
    audio_completion_tokens: 0, image_completion_tokens: 0,
  }, pricing_snapshot: { mode: 'ratio', final_unit_price_input_per_1m_usd: 2,
    cache_write_ratio: '1.25', modality_ratios: { cache_write_5m: '1.25', cache_write_1h: '2' } } } as LogRow
  const writes = billingLines(row).filter((l) => l.name.startsWith('cacheWrite'))
  expect(writes.map((l) => [l.quantity, l.amountMicro])).toEqual([[60, 150], [40, 160]])
  expect(billingLines({ ...row, usage: { ...row.usage, cache_write_1h_tokens: null } })).toEqual([])
})

test('广场展示明确不支持和独立时长单价，模拟器分档且不重复累计', async ({ page }) => {
  await setup(page, [{ ...existing, pricing_mode: 'ratio' }])
  await page.goto('/pricing?model=custom-vision')
  const d = page.getByRole('dialog')
  await expect(d.getByText('工具调用 · 不支持', { exact: true })).toBeVisible()
  await expect(d.getByText('图像理解', { exact: true })).toBeVisible()
  await expect(d.getByText('1 小时缓存写入', { exact: true })).toBeVisible()
  await d.locator('#sim-cache_write_5m').fill('60')
  await d.locator('#sim-cache_write_1h').fill('40')
  const simulator = d.locator('section').filter({ has: page.locator('#sim-cache_write_5m') })
  await expect(simulator.locator('strong')).toHaveText(/0\.00611/)
  await d.locator('#sim-cacheWrite').fill('1000')
  await expect(d.getByRole('alert')).toBeVisible()
})
