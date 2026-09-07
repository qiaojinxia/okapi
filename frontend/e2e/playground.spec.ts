import { expect, test } from '@playwright/test'
import type { Page, Route } from '@playwright/test'
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
function sseBody(): string {
  const chunk = (delta: object, extra: object = {}) =>
    `data: ${JSON.stringify({ id: 'c1', object: 'chat.completion.chunk', model: 'gpt-4o-mock', choices: [{ index: 0, delta, finish_reason: null }], ...extra })}\n\n`
  return (
    chunk({ role: 'assistant', content: 'Hello' }) +
    chunk({ content: ' playground' }) +
    `data: ${JSON.stringify({ id: 'c1', object: 'chat.completion.chunk', model: 'gpt-4o-mock', choices: [], usage: { prompt_tokens: 100, completion_tokens: 20, total_tokens: 120 } })}\n\n` +
    'data: [DONE]\n\n'
  )
}

interface Options {
  /// 中继端点是否挂起不回（测停止按钮）。
  hang?: boolean
}

async function prepare(page: Page, opts: Options = {}) {
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
      if (opts.hang) {
        // 挂住到测试点停止：abort 会让前端 fetch 直接失败，这里久等后回错误体兜底
        await new Promise((r) => setTimeout(r, 5_000))
        await route.fulfill({ status: 503, contentType: 'application/json', body: JSON.stringify({ error: { code: 'upstream_error' } }) })
        return
      }
      await route.fulfill({ status: 200, headers: { 'content-type': 'text/event-stream' }, body: sseBody() })
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
    const json =
      path === '/api/me' ? ME
      : path === '/api/pricing' ? PRICING
      : path === '/api/playground/presets' ? SITE_PRESETS
      : path === '/api/me/keys' ? { total: 0, data: [] }
      : path === '/api/me/groups' ? { current: 'vip', selectable: [] }
      : path === '/api/notice' ? { notice: null }
      : { data: [], next_before: null }
    await route.fulfill({ json })
  })
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
