import { expect, test } from '@playwright/test'
import type { Page, Route } from '@playwright/test'
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

// Playground 试用台（IMPLEMENTATION §11.39）：接口桩 + SSE 桩。
// 覆盖：模型下拉只列本分组可用、发送 → 流式内容 + usage 脚注、停止按钮中断、
// 预设保存 / 载入 / 站点预设导入、密钥回执上的一键导入链接形状。

const ME = {
  user_id: 7, key_id: 1, group: 'vip', balance_micro: 5_000_000, balance_expires_at: null,
  subscription_remaining_micro: 0, subscription_until_unix: 0, role: 1, permissions: [],
}
const PRICING = {
  groups: [{ code: 'default', name: null, ratio: '1' }, { code: 'vip', name: 'VIP', ratio: '0.8' }],
  models: [
    { model: 'gpt-5', display_name: 'GPT-5', vendor: 'OpenAI', mode: 'ratio', model_ratio: '1.25', completion_ratio: '8', cache_ratio: '0.1', cache_write_ratio: null, audio_ratio: null, audio_completion_ratio: null, image_ratio: null, per_call_price_micro: null, groups: ['default', 'vip'] },
    { model: 'claude-sonnet-4', display_name: 'Claude Sonnet 4', vendor: 'Anthropic', mode: 'ratio', model_ratio: '1.5', completion_ratio: '5', cache_ratio: '0.1', cache_write_ratio: '1.25', audio_ratio: null, audio_completion_ratio: null, image_ratio: null, per_call_price_micro: null, groups: ['default'] },
  ],
}
const SITE_PRESETS = {
  data: [
    { name: 'Site Writer', model: 'gpt-5', system: 'Write clearly.', temperature: 0.5, max_tokens: 256, top_p: 0.9 },
  ],
}

/// SSE 主体：两块内容 + usage + [DONE]（一次性回，前端解析逻辑一致）。
function sseBody(reply?: string): string {
  const chunk = (delta: object, extra: object = {}) =>
    `data: ${JSON.stringify({ id: 'c1', object: 'chat.completion.chunk', model: 'gpt-4o-mock', choices: [{ index: 0, delta, finish_reason: null }], ...extra })}\n\n`
  // 自定义回复按 9 个字符一块切开，让围栏 / 加粗在块边界被截断，覆盖流式残缺文本的渲染
  const pieces = reply === undefined ? ['Hello', ' playground'] : (reply.match(/[\s\S]{1,9}/g) ?? [])
  return (
    pieces.map((piece, index) => chunk(index === 0 ? { role: 'assistant', content: piece } : { content: piece })).join('') +
    `data: ${JSON.stringify({ id: 'c1', object: 'chat.completion.chunk', model: 'gpt-4o-mock', choices: [], usage: { prompt_tokens: 100, completion_tokens: 20, total_tokens: 120 } })}\n\n` +
    'data: [DONE]\n\n'
  )
}

interface Options {
  /// 中继端点是否挂起不回（测停止按钮）。
  hang?: boolean
  /// 助手回复正文（缺省 "Hello playground"）。
  reply?: string
  /// `/api/me/keys` 的数据（缺省空列表）；`keysDelayMs` 让它延迟返回。
  keys?: unknown[]
  keysDelayMs?: number
}

/// 试用台"使用的密钥"候选：账号分组是 vip（只放行 gpt-5），ci-bot 钉在 default 分组并只许 claude-sonnet-4。
const KEYS = [
  { id: 11, name: 'ci-bot', key_prefix: 'sk-okapi-ci', copy_status: 'available', status: 1, model_allowlist: ['claude-sonnet-4'], group_override: 'default', expires_at: null, quota_mode: 1, quota_micro: 5_000_000, used_micro: 1_250_000 },
  { id: 12, name: 'open-key', key_prefix: 'sk-okapi-op', copy_status: 'available', status: 1, model_allowlist: null, group_override: null, expires_at: null, quota_mode: 0, quota_micro: null, used_micro: 0 },
  { id: 13, name: 'legacy', key_prefix: 'sk-okapi-lg', copy_status: 'not_saved', status: 1, model_allowlist: null, group_override: null, expires_at: null, quota_mode: 0, quota_micro: null, used_micro: 0 },
  { id: 14, name: 'paused', key_prefix: 'sk-okapi-pa', copy_status: 'available', status: 2, model_allowlist: null, group_override: null, expires_at: null, quota_mode: 0, quota_micro: null, used_micro: 0 },
  { id: 15, name: 'old', key_prefix: 'sk-okapi-ol', copy_status: 'available', status: 1, model_allowlist: null, group_override: null, expires_at: '2020-01-01T00:00:00Z', quota_mode: 0, quota_micro: null, used_micro: 0 },
]

/// 返回中继收到的请求体列表（按到达顺序），供断言历史 / 参数。
async function prepare(page: Page, opts: Options = {}) {
  const bodies: Array<{ messages: Array<{ role: string; content: string }>; model: string; temperature: number; keyHeader: string | null }> = []
  await page.addInitScript(() => {
    localStorage.setItem('okapi.key', 'interaction-test-key')
    localStorage.setItem('okapi.lang', 'en')
  })
  await page.route('**/*', async (route: Route) => {
    const request = route.request()
    const path = new URL(request.url()).pathname
    if (request.isNavigationRequest()) {
      await route.fulfill({ path: fileURLToPath(new URL('../dist/index.html', import.meta.url)), contentType: 'text/html' })
      return
    }
    if (path === '/api/me/playground/chat') {
      expect(request.method()).toBe('POST')
      // 中继必然被强制为流式
      expect(JSON.parse(request.postData() ?? '{}').stream).toBe(true)
      bodies.push({ ...JSON.parse(request.postData() ?? '{}'), keyHeader: request.headers()['x-okapi-playground-key'] ?? null })
      if (opts.hang) {
        // 挂住到测试点停止：abort 会让前端 fetch 直接失败，这里久等后回错误体兜底
        await new Promise((r) => setTimeout(r, 5_000))
        await route.fulfill({ status: 503, contentType: 'application/json', body: JSON.stringify({ error: { code: 'upstream_error' } }) })
        return
      }
      await route.fulfill({ status: 200, headers: { 'content-type': 'text/event-stream' }, body: sseBody(opts.reply) })
      return
    }
    if (path === '/auth/keys' && request.method() === 'POST') {
      await route.fulfill({ json: { key_id: 2, api_key: 'sk-okapi-minted-xyz' } })
      return
    }
    // 静态资源（/assets/*.js 等）放行给预览服务器，只桩后端路径——否则模块脚本被兜底成 JSON 会白屏
    if (!/^\/(api|admin|auth|pay)\//.test(path)) {
      await route.continue()
      return
    }
    if (path === '/api/me/keys' && opts.keysDelayMs) await new Promise((r) => setTimeout(r, opts.keysDelayMs))
    const json =
      path === '/api/me' ? ME
      : path === '/api/pricing' ? PRICING
      : path === '/api/playground/presets' ? SITE_PRESETS
      : path === '/api/me/keys' ? { total: opts.keys?.length ?? 0, data: opts.keys ?? [] }
      : path === '/api/me/groups' ? { current: 'vip', selectable: [] }
      : path === '/api/notice' ? { notice: null }
      : { data: [], next_before: null }
    await route.fulfill({ json })
  })
  return bodies
}

test('模型下拉只列本分组可用，发送后流式内容与 usage 脚注出现', async ({ page }) => {
  await prepare(page)
  await page.goto('/portal/playground')

  // vip 分组可用的只有 gpt-5（claude 仅 default）；输入框默认选它，datalist 不含 claude
  const model = page.getByLabel('Model ID', { exact: true })
  await expect(model).toHaveValue('gpt-5')
  const options = await page.locator('datalist option').evaluateAll((els) => els.map((e) => (e as HTMLOptionElement).value))
  expect(options).toEqual(['gpt-5'])

  const input = page.getByPlaceholder(/Enter to send/)
  await input.fill('hi there')
  await page.getByRole('button', { name: 'Send' }).click()

  const assistant = page.locator('[data-role="assistant"]')
  await expect(assistant).toContainText('Hello playground')
  await expect(assistant).toContainText('100 in · 20 out')
  await expect(assistant).toContainText('gpt-4o-mock')
  // 收尾后回到可发送态
  await expect(page.getByRole('button', { name: 'Send' })).toBeVisible()

  await page.route('**/api/me/playground/chat', (route) =>
    route.fulfill({ status: 500, json: { error: { code: 'internal_error' } } }),
  )
  await input.fill('fail me')
  await page.getByRole('button', { name: 'Send' }).click()
  await expect(page.getByRole('alert').filter({ hasText: 'Internal error, please retry later' })).toBeVisible()
})

test('停止按钮中断在途流式', async ({ page }) => {
  await prepare(page, { hang: true })
  await page.goto('/portal/playground')
  await page.getByPlaceholder(/Enter to send/).fill('slow one')
  await page.getByRole('button', { name: 'Send' }).click()

  const stop = page.getByRole('button', { name: 'Stop' })
  await expect(stop).toBeVisible()
  await stop.click()
  // 中断即回到可发送态，且不落错误（是用户主动停的）
  await expect(page.getByRole('button', { name: 'Send' })).toBeVisible()
  await expect(page.getByRole('alert')).toHaveCount(0)
})

test('预设保存 / 载入，站点预设一键导入', async ({ page }) => {
  await prepare(page)
  await page.goto('/portal/playground')

  await page.getByLabel('System prompt').fill('be terse')
  await page.getByLabel('Preset name').fill('Mine')
  await page.getByRole('button', { name: 'Save' }).click()
  // 保存后出现在"我的预设"里
  const mine = page.getByRole('button', { name: /Mine/ })
  await expect(mine).toBeVisible()
  expect(await page.evaluate(() => localStorage.getItem('okapi.playground.7'))).toContain('Mine')

  // 改动后点预设名载入回来
  await page.getByLabel('System prompt').fill('changed')
  await mine.click()
  await expect(page.getByLabel('System prompt')).toHaveValue('be terse')

  // 站点预设一键导入 = 应用到表单 + 存为本地预设
  await page.getByRole('button', { name: 'Import' }).click()
  await expect(page.getByLabel('System prompt')).toHaveValue('Write clearly.')
  await expect(page.getByRole('button', { name: /Site Writer/ })).toBeVisible()
  expect(await page.evaluate(() => localStorage.getItem('okapi.playground.7'))).toContain('Site Writer')
})

test('密钥回执上的一键导入链接形状正确', async ({ page }) => {
  await prepare(page)
  await page.goto('/portal/keys')
  // 页头与空态各有一个 "New key"：列表桩回空后空态才出现，按钮数量随时序在 1 / 2 之间跳，取第一个
  await page.getByRole('button', { name: 'New key' }).first().click()
  // 抽屉开后 30ms 才把焦点送进第一个输入框；等它进去再填，否则并行负载下 fill 会被打断
  await expect(page.getByRole('dialog').locator(':focus')).toHaveCount(1)
  await page.getByLabel('Name', { exact: true }).fill('cli')
  await page.getByRole('button', { name: 'Create' }).click()

  // 回执上"How to connect"把明文带进指南；指南里出现四个导入链接
  await page.getByRole('button', { name: 'How to connect' }).click()
  const links = page.getByTestId('import-links')
  await expect(links).toBeVisible()
  const href = async (name: RegExp) => links.getByRole('link', { name }).getAttribute('href')

  // cc-switch：Claude 走不带 /v1 的基址、Codex 走带 /v1 的基址
  const claude = await href(/cc-switch · Claude/)
  expect(claude).toContain('ccswitch://v1/import?')
  expect(claude).toContain('app=claude')
  expect(claude).toContain('endpoint=http%3A%2F%2F127.0.0.1%3A8080')
  expect(claude).not.toContain('8080%2Fv1')
  expect(claude).toContain('apiKey=sk-okapi-minted-xyz')

  const codex = await href(/cc-switch · Codex/)
  expect(codex).toContain('app=codex')
  expect(codex).toContain('endpoint=http%3A%2F%2F127.0.0.1%3A8080%2Fv1')

  const nextchat = await href(/NextChat/)
  expect(nextchat).toContain('https://app.nextchat.club/#/?settings=')
  expect(nextchat).toContain(encodeURIComponent('"key":"sk-okapi-minted-xyz"'))

  const cherry = await href(/Cherry Studio/)
  expect(cherry).toContain('cherrystudio://providers/api-keys?v=1&data=')
})

const send = async (page: Page, text: string) => {
  await page.getByPlaceholder(/Enter to send/).fill(text)
  await page.getByRole('button', { name: 'Send' }).click()
}

test('回复按 markdown 渲染：代码块可复制、加粗 / 行内代码 / 表格，链接只放行 http(s)', async ({ page }) => {
  const reply = [
    'Here is **O(n log n)** and `inline_code`:',
    '',
    '```rust',
    'fn main() {',
    '    println!("hi");',
    '}',
    '```',
    '',
    '- first item',
    '- second [docs](https://example.com/docs) item',
    '',
    '| Model | Price |',
    '| --- | --- |',
    '| gpt-5 | $2 |',
    '',
    '[bad](javascript:alert(1)) and <img src=x onerror=alert(1)>',
  ].join('\n')
  await prepare(page, { reply })
  await page.goto('/portal/playground')
  await send(page, 'show me')

  const assistant = page.locator('[data-role="assistant"]')
  await expect(assistant.locator('strong')).toHaveText('O(n log n)')
  await expect(assistant.locator('p code')).toHaveText('inline_code')
  const pre = assistant.locator('pre code')
  await expect(pre).toHaveText('fn main() {\n    println!("hi");\n}')
  await expect(assistant.getByText('rust', { exact: true })).toBeVisible()
  await expect(assistant.locator('ul > li')).toHaveCount(2)
  await expect(assistant.getByRole('columnheader')).toHaveText(['Model', 'Price'])
  await expect(assistant.getByRole('cell')).toHaveText(['gpt-5', '$2'])

  // 链接：http(s) 新窗口且 noopener；javascript: 不成链接，HTML 当文本显示而不是被执行
  const docs = assistant.getByRole('link', { name: 'docs' })
  await expect(docs).toHaveAttribute('href', 'https://example.com/docs')
  await expect(docs).toHaveAttribute('target', '_blank')
  await expect(docs).toHaveAttribute('rel', /noopener/)
  await expect(assistant.locator('a[href^="javascript"]')).toHaveCount(0)
  await expect(assistant.locator('img')).toHaveCount(0)
  await expect(assistant).toContainText('<img src=x onerror=alert(1)>')

  await page.context().grantPermissions(['clipboard-read', 'clipboard-write'])
  await assistant.getByRole('button', { name: 'Copy code' }).click()
  await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toBe('fn main() {\n    println!("hi");\n}')
  await assistant.getByRole('button', { name: 'Copy reply' }).click()
  await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toBe(reply)
})

test('回复脚注：首字耗时与按当前分组价目估算的费用；用户消息不带脚注', async ({ page }) => {
  await prepare(page)
  await page.goto('/portal/playground')
  await send(page, 'hi there')
  const assistant = page.locator('[data-role="assistant"]')
  await expect(assistant).toContainText('100 in · 20 out')
  await expect(assistant).toContainText(/TTFT \d+ ms/)
  // vip 分组倍率 0.8：gpt-5 输入 1.25×0.8×$2 = $2/1M、输出 ×8 = $16/1M；100 入 + 20 出 = $0.00052
  await expect(assistant.getByText(/^≈ \$0\.0005$/)).toBeVisible()
  await expect(page.locator('[data-role="user"]')).toHaveText('hi there')
  await expect(page.locator('[data-role="user"] [aria-label]')).toHaveCount(0)
})

test('重新生成：用同一句话再问一次，旧回复被替换且历史里不含它', async ({ page }) => {
  const bodies = await prepare(page)
  await page.goto('/portal/playground')
  await send(page, 'first question')
  await expect(page.locator('[data-role="assistant"]')).toContainText('Hello playground')
  await send(page, 'second question')
  await expect(page.locator('[data-role="assistant"]')).toHaveCount(2)
  expect(bodies).toHaveLength(2)

  // 只有最后一条回复有"重新生成"
  await expect(page.getByRole('button', { name: 'Regenerate' })).toHaveCount(1)
  await page.getByRole('button', { name: 'Regenerate' }).click()
  await expect.poll(() => bodies.length).toBe(3)
  // 第三次请求：第一轮完整历史 + 同一句 second question；上一条 second 的回复不再出现
  expect(bodies[2].messages.map((m) => `${m.role}:${m.content}`)).toEqual([
    'user:first question', 'assistant:Hello playground', 'user:second question',
  ])
  await expect(page.locator('[data-role="user"]')).toHaveText(['first question', 'second question'])
  await expect(page.locator('[data-role="assistant"]')).toHaveCount(2)
})

test('失败的回复可以重试；重试成功后错误消失', async ({ page }) => {
  await prepare(page)
  let fail = true
  await page.route('**/api/me/playground/chat', async (route) => {
    if (fail) return route.fulfill({ status: 500, json: { error: { code: 'internal_error' } } })
    return route.fulfill({ status: 200, headers: { 'content-type': 'text/event-stream' }, body: sseBody() })
  })
  await page.goto('/portal/playground')
  await send(page, 'will fail')
  await expect(page.getByRole('alert').filter({ hasText: 'Internal error, please retry later' })).toBeVisible()
  fail = false
  await page.getByRole('button', { name: 'Retry' }).click()
  await expect(page.locator('[data-role="assistant"]')).toContainText('Hello playground')
  await expect(page.getByRole('alert')).toHaveCount(0)
  await expect(page.locator('[data-role="user"]')).toHaveCount(1)
})

test('刷新后对话与参数仍在，清空后不再恢复', async ({ page }) => {
  await prepare(page)
  await page.goto('/portal/playground')
  await page.getByLabel('System prompt').fill('be terse')
  await page.getByLabel('temperature').fill('0.3')
  await send(page, 'remember me')
  await expect(page.locator('[data-role="assistant"]')).toContainText('Hello playground')

  await page.reload()
  await expect(page.locator('[data-role="user"]')).toHaveText('remember me')
  await expect(page.locator('[data-role="assistant"]')).toContainText('Hello playground')
  await expect(page.locator('[data-role="assistant"]')).toContainText('100 in · 20 out')
  await expect(page.getByLabel('System prompt')).toHaveValue('be terse')
  await expect(page.getByLabel('temperature')).toHaveValue('0.3')
  // 恢复出来的不是"生成中"：能直接发下一条
  await expect(page.getByRole('button', { name: 'Send' })).toBeVisible()

  await page.getByRole('button', { name: 'Clear conversation' }).click()
  await expect(page.locator('[data-role]')).toHaveCount(0)
  await page.reload()
  await expect(page.locator('[data-role]')).toHaveCount(0)
  await expect(page.getByLabel('System prompt')).toHaveValue('be terse')
})

test('模型信息卡：厂商 / 本分组价目 / 上下文；目录外的模型给出提示而不是空白', async ({ page }) => {
  await prepare(page)
  await page.goto('/portal/playground')
  const card = page.locator('[data-slot="playground-model-info"]')
  await expect(card).toContainText('GPT-5')
  await expect(card).toContainText('OpenAI')
  await expect(card.getByText('Input / 1M').locator('..')).toContainText('$2.00')
  await expect(card.getByText('Output / 1M').locator('..')).toContainText('$16.00')
  await expect(card.getByRole('link', { name: /View GPT-5 in the model catalog/ })).toHaveAttribute('href', /\/pricing\?.*model=gpt-5/)

  await page.getByLabel('Model ID', { exact: true }).fill('my-custom-model')
  await expect(card).toHaveCount(0)
  await expect(page.getByText('not in the catalog')).toBeVisible()
  // 仍然可以发：手输的模型原样请求
  await expect(page.getByPlaceholder(/Enter to send/)).toBeVisible()
})

test('空态示例问题填入输入框但不自动发送', async ({ page }) => {
  const bodies = await prepare(page)
  await page.goto('/portal/playground')
  const starters = page.getByRole('list', { name: 'Try one of these' }).getByRole('button')
  await expect(starters).toHaveCount(3)
  const text = (await starters.nth(1).textContent()) ?? ''
  await starters.nth(1).click()
  await expect(page.getByPlaceholder(/Enter to send/)).toHaveValue(text)
  await expect(page.getByPlaceholder(/Enter to send/)).toBeFocused()
  expect(bodies).toHaveLength(0)
  await page.keyboard.press('Enter')
  await expect(page.locator('[data-role="assistant"]')).toContainText('Hello playground')
  expect(bodies[0].messages.at(-1)?.content).toBe(text)
})

test('导出对话为 Markdown：含双方原文与模型脚注，失败的回复不导出', async ({ page }) => {
  await prepare(page)
  await page.goto('/portal/playground')
  await expect(page.getByRole('button', { name: 'Export conversation' })).toBeDisabled()
  await send(page, 'hi there')
  await expect(page.locator('[data-role="assistant"]')).toContainText('Hello playground')
  const [download] = await Promise.all([page.waitForEvent('download'), page.getByRole('button', { name: 'Export conversation' }).click()])
  expect(download.suggestedFilename()).toMatch(/^playground-\d{12}\.md$/)
  const text = readFileSync((await download.path())!, 'utf8')
  expect(text).toBe('# Playground conversation\n\n## User\n\nhi there\n\n## Assistant\n\nHello playground\n\n> gpt-4o-mock · 100 in · 20 out\n')
})

test('窄屏：配置栏默认收起、可展开；对话与输入框不超出屏幕', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 860 })
  await prepare(page, { reply: '```\nconst veryLongLine = "x".repeat(200); console.log(veryLongLine, veryLongLine)\n```' })
  await page.goto('/portal/playground')
  const model = page.getByLabel('Model ID', { exact: true })
  await expect(model).toBeHidden()
  const toggle = page.getByRole('button', { name: /Model & parameters/ })
  await expect(toggle).toHaveAttribute('aria-expanded', 'false')
  await toggle.click()
  await expect(model).toBeVisible()
  await expect(toggle).toHaveAttribute('aria-expanded', 'true')
  await toggle.click()
  await expect(model).toBeHidden()

  await send(page, 'long code please')
  await expect(page.locator('[data-role="assistant"] pre')).toBeVisible()
  // 长代码行在代码块内横向滚动，不把页面撑宽；输入框与发送按钮都在屏内
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth)
  expect(overflow).toBeLessThanOrEqual(0)
  const box = await page.getByRole('button', { name: 'Send' }).boundingBox()
  expect(box!.x + box!.width).toBeLessThanOrEqual(390)
})

const keySelect = (page: Page) => page.getByLabel('API key to use')

test('选用密钥：登录会话为默认，停用 / 过期 / 未保存明文的密钥列出但不可选，并说明原因', async ({ page }) => {
  await prepare(page, { keys: KEYS })
  await page.goto('/portal/playground')
  const select = keySelect(page)
  await expect(select).toHaveValue('')
  await expect(select.locator('option')).toHaveText([
    'Login session (default)',
    'ci-bot · sk-okapi-ci…',
    'open-key · sk-okapi-op…',
    'legacy · sk-okapi-lg… — secret not saved',
    'paused · sk-okapi-pa… — disabled',
    'old · sk-okapi-ol… — expired',
  ])
  for (const name of [/^legacy/, /^paused/, /^old/]) await expect(select.getByRole('option', { name })).toBeDisabled()
  await expect(select.getByRole('option', { name: /^ci-bot/ })).toBeEnabled()
  await expect(page.getByText('cannot be used here')).toBeVisible()
  // 没选密钥时不显示密钥摘要
  await expect(page.locator('[data-slot="playground-key-info"]')).toHaveCount(0)
})

test('选用密钥：模型候选按该密钥的分组与白名单收窄，请求带上密钥 id，脚注标明用的哪把并按它的分组估价', async ({ page }) => {
  const bodies = await prepare(page, { keys: KEYS })
  await page.goto('/portal/playground')
  const model = page.getByLabel('Model ID', { exact: true })
  const candidates = () => page.locator('datalist option').evaluateAll((els) => els.map((e) => (e as HTMLOptionElement).value))
  // 账号分组 vip：只有 gpt-5
  await expect(model).toHaveValue('gpt-5')
  expect(await candidates()).toEqual(['gpt-5'])

  await keySelect(page).selectOption('11')
  // ci-bot 钉 default 分组 + 白名单只含 claude-sonnet-4：候选收窄，当前模型自动换成它
  expect(await candidates()).toEqual(['claude-sonnet-4'])
  await expect(model).toHaveValue('claude-sonnet-4')
  const info = page.locator('[data-slot="playground-key-info"]')
  await expect(info).toContainText('Group default')
  await expect(info).toContainText('1 allowed models')
  await expect(info).toContainText('$3.75 left')
  await expect(page.locator('[data-slot="playground-model-info"]')).toContainText('Claude Sonnet 4')

  await send(page, 'via key')
  const assistant = page.locator('[data-role="assistant"]')
  await expect(assistant).toContainText('Hello playground')
  expect(bodies[0].keyHeader).toBe('11')
  expect(bodies[0].model).toBe('claude-sonnet-4')
  await expect(assistant).toContainText('Key ci-bot')
  // default 分组倍率 1：claude-sonnet-4 输入 1.5×$2 = $3/1M、输出 ×5 = $15/1M；100 入 + 20 出 = $0.0006（vip 倍率 0.8 时会是 $0.00048）
  await expect(assistant.getByText(/^≈ \$0\.0006$/)).toBeVisible()

  // 换回登录会话：claude 在 vip 下不可用，模型回到 gpt-5；请求不再带密钥头
  await keySelect(page).selectOption('')
  await expect(model).toHaveValue('gpt-5')
  expect(await candidates()).toEqual(['gpt-5'])
  await send(page, 'via login')
  await expect.poll(() => bodies.length).toBe(2)
  expect(bodies[1].keyHeader).toBeNull()
})

test('选用密钥：未钉分组且无白名单的密钥跟随账号分组；手输的目录外模型换密钥时保留；目录里有但此密钥用不了时给出警告', async ({ page }) => {
  await prepare(page, { keys: KEYS })
  await page.goto('/portal/playground')
  const model = page.getByLabel('Model ID', { exact: true })
  await keySelect(page).selectOption('12')
  await expect(page.locator('[data-slot="playground-key-info"]')).toContainText('Group vip')
  await expect(page.locator('[data-slot="playground-key-info"]')).toContainText('All models')
  expect(await page.locator('datalist option').evaluateAll((els) => els.map((e) => (e as HTMLOptionElement).value))).toEqual(['gpt-5'])

  await model.fill('claude-sonnet-4')
  await expect(page.getByRole('status').filter({ hasText: 'cannot call this model' })).toBeVisible()
  await model.fill('gpt-5')
  await expect(page.getByRole('status').filter({ hasText: 'cannot call this model' })).toHaveCount(0)

  await model.fill('my-custom-model')
  await keySelect(page).selectOption('11')
  await expect(model).toHaveValue('my-custom-model')
})

test('选用密钥：选择刷新后保留；已被删除 / 停用的密钥回到登录会话并提示；密钥列表失败时只剩登录会话', async ({ page }) => {
  await prepare(page, { keys: KEYS })
  await page.goto('/portal/playground')
  await keySelect(page).selectOption('11')
  await page.reload()
  await expect(keySelect(page)).toHaveValue('11')
  await expect(page.getByLabel('Model ID', { exact: true })).toHaveValue('claude-sonnet-4')

  // 上次选的 ci-bot 现在被停用了
  await page.route('**/api/me/keys?*', (route) => route.fulfill({ json: { total: 1, data: [{ ...KEYS[0], status: 2 }] } }))
  await page.reload()
  await expect(keySelect(page)).toHaveValue('')
  await expect(page.getByText('no longer available')).toBeVisible()
  // 模型本是"跟随该密钥的第一个可用模型"，回到登录会话（vip 分组）后跟随 gpt-5
  await expect(page.getByLabel('Model ID', { exact: true })).toHaveValue('gpt-5')

  // 列表接口失败：降级为只用登录会话，页面照常可用
  await page.route('**/api/me/keys?*', (route) => route.fulfill({ status: 500, json: { error: { code: 'internal_error' } } }))
  await page.reload()
  await expect(keySelect(page).locator('option')).toHaveText(['Login session (default)'])
  await expect(page.getByPlaceholder(/Enter to send/)).toBeVisible()
})

test('选用密钥：密钥列表还没回来时不能发送，避免把本该用选定密钥的请求悄悄走成登录会话', async ({ page }) => {
  await page.addInitScript(() => {
    localStorage.setItem('okapi.playground.settings.7', JSON.stringify({ model: 'claude-sonnet-4', system: '', temperature: '1', topP: '1', maxTokens: '', keyId: '11' }))
  })
  const bodies = await prepare(page, { keys: KEYS, keysDelayMs: 1200 })
  await page.goto('/portal/playground')
  await page.getByPlaceholder(/Enter to send/).fill('too early')
  await expect(page.getByRole('button', { name: 'Send' })).toBeDisabled()
  await expect(page.getByRole('button', { name: 'Send' })).toBeEnabled({ timeout: 10_000 })
  await page.getByRole('button', { name: 'Send' }).click()
  await expect(page.locator('[data-role="assistant"]')).toContainText('Hello playground')
  expect(bodies).toHaveLength(1)
  expect(bodies[0].keyHeader).toBe('11')
})
