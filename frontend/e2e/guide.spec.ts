import { expect, test } from '@playwright/test'
import type { Page } from '@playwright/test'
import { fileURLToPath } from 'node:url'

// 新手引导：接口桩下验证进度推导、抽屉四步、客户端片段联动与关闭记忆；不发起任何写请求。
async function prepare(page: Page, { called = false, language = 'zh-CN' }: { called?: boolean; language?: string } = {}) {
  await page.addInitScript((lang) => {
    localStorage.setItem('okapi.key', 'interaction-test-key')
    localStorage.setItem('okapi.lang', lang)
  }, language)
  await page.route('**/*', async (route) => {
    const request = route.request()
    const path = new URL(request.url()).pathname
    if (request.isNavigationRequest()) {
      await route.fulfill({ path: fileURLToPath(new URL('../dist/index.html', import.meta.url)), contentType: 'text/html' })
    } else if (/^\/(api|admin|auth|pay)\//.test(path)) {
      expect(request.method(), '引导回归不应提交业务修改').toBe('GET')
      const json = path === '/api/me' ? {
        user_id: 7, key_id: 1, group: 'vip', balance_micro: 5_000_000, balance_expires_at: null,
        subscription_remaining_micro: 0, subscription_until_unix: 0, role: 1, permissions: [],
      } : path === '/api/me/keys' ? {
        total: 1,
        data: [{
          id: 1, name: 'login', key_prefix: 'sk-okapi-abc', status: 1, used_micro: called ? 12_500 : 0, requests: 0,
          rpm_limit: null, created_at: '2026-09-01T00:00:00Z', amount_micro: 0, group_override: null, ip_allowlist: null,
          last_used_at: called ? '2026-09-05T08:00:00Z' : null,
        }],
      } : path === '/api/pricing' ? {
        groups: [{ code: 'default', name: null, ratio: '1' }, { code: 'vip', name: 'VIP', ratio: '0.8' }],
        models: [
          { model: 'gpt-5', display_name: 'GPT-5', vendor: 'OpenAI', mode: 'ratio', model_ratio: '1.25', completion_ratio: '8', cache_ratio: '0.1', cache_write_ratio: null, audio_ratio: null, audio_completion_ratio: null, image_ratio: null, per_call_price_micro: null, groups: ['default', 'vip'] },
          { model: 'claude-sonnet-4', display_name: 'Claude Sonnet 4', vendor: 'Anthropic', mode: 'ratio', model_ratio: '1.5', completion_ratio: '5', cache_ratio: '0.1', cache_write_ratio: '1.25', audio_ratio: null, audio_completion_ratio: null, image_ratio: null, per_call_price_micro: null, groups: ['default'] },
        ],
      } : path === '/api/notice' ? { notice: null } : { data: [], next_before: null }
      await route.fulfill({ json })
    } else {
      await route.continue()
    }
  })
}

test('新用户总览出现快速开始卡，抽屉四步与客户端片段联动，关闭后不再出现', async ({ page }) => {
  await prepare(page)
  await page.goto('/portal')
  const card = page.getByRole('region', { name: '快速开始' })
  await expect(card).toBeVisible()
  await expect(card).toContainText('已完成 1 / 4 步')
  await expect(card.getByRole('list', { name: '接入步骤' }).getByRole('listitem')).toHaveCount(4)
  await page.screenshot({ path: 'test-results/guide-card.png', animations: 'disabled' })

  await card.getByRole('button', { name: '打开接入指南' }).click()
  const dialog = page.getByRole('dialog', { name: '快速开始' })
  await expect(dialog).toBeVisible()
  await expect(dialog.getByRole('list', { name: '接入步骤' }).getByRole('listitem')).toHaveCount(4)
  await expect(dialog.getByText('已有 1 把密钥')).toBeVisible()
  await expect(dialog.getByRole('link', { name: '打开模型广场' })).toHaveAttribute('href', /\/pricing\?.*available=true/)
  await expect(dialog.getByRole('link', { name: '打开模型广场' })).toHaveAttribute('href', /group=vip/)

  // 模型候选只列 vip 分组已接入的模型，默认选中它；Base URL 按本地预览端口映射到网关
  const model = dialog.getByLabel('模型 ID', { exact: true })
  await expect(model).toHaveValue('gpt-5')
  await expect(dialog.getByLabel('接口基础地址（Base URL）')).toHaveValue('http://127.0.0.1:8080/v1')
  const code = dialog.locator('pre')
  await expect(code).toHaveCount(2)
  await expect(code.first()).toContainText("export OKAPI_API_KEY='YOUR_API_KEY'")
  await expect(code.nth(1)).toContainText('http://127.0.0.1:8080/v1/chat/completions')
  await expect(code.nth(1)).toContainText('"model": "gpt-5"')

  const clients = dialog.getByRole('group', { name: '客户端' })
  await clients.getByRole('button', { name: 'Claude Code' }).click()
  await expect(code.first()).toContainText("export ANTHROPIC_BASE_URL='http://127.0.0.1:8080'")
  await expect(code.first()).not.toContainText('8080/v1')
  await expect(code.first()).toContainText("ANTHROPIC_DEFAULT_HAIKU_MODEL='gpt-5'")
  await expect(dialog.getByText('~/.claude/settings.json', { exact: true })).toBeVisible()
  await expect(code.nth(1)).toContainText('"ANTHROPIC_AUTH_TOKEN": "YOUR_API_KEY"')
  await page.screenshot({ path: 'test-results/guide-drawer-claude.png', animations: 'disabled' })

  await clients.getByRole('button', { name: 'Codex CLI' }).click()
  await expect(dialog.getByText('~/.codex/config.toml', { exact: true })).toBeVisible()
  await expect(code.nth(1)).toContainText('wire_api = "responses"')
  await expect(code.nth(1)).toContainText('base_url = "http://127.0.0.1:8080/v1"')

  // 改地址与模型，片段随之更新；非法地址给出提示而不是生成错误配置
  await model.fill('my-custom-model')
  await dialog.getByLabel('接口基础地址（Base URL）').fill('https://api.example.com/okapi')
  await expect(code.nth(1)).toContainText('base_url = "https://api.example.com/okapi/v1"')
  await expect(code.nth(1)).toContainText('model = "my-custom-model"')
  await clients.getByRole('button', { name: 'Python' }).click()
  await expect(code.nth(1)).toContainText('base_url="https://api.example.com/okapi/v1"')
  await expect(code.nth(1)).toContainText('os.environ["OKAPI_API_KEY"]')
  await clients.getByRole('button', { name: '通用客户端' }).click()
  await expect(dialog.locator('pre')).toHaveCount(0)
  await expect(dialog.locator('dl')).toContainText('OpenAI 兼容')
  await expect(dialog.locator('dl')).toContainText('https://api.example.com/okapi/v1')
  await expect(dialog.locator('dl')).toContainText('YOUR_API_KEY')
  await dialog.getByLabel('接口基础地址（Base URL）').fill('not a url')
  await expect(dialog.getByText(/请输入有效的 HTTP\(S\) 网关地址/)).toBeVisible()
  await expect(dialog.locator('dl')).toHaveCount(0)

  await dialog.getByRole('button', { name: '稍后再看' }).click()
  await expect(dialog).toHaveCount(0)
  await expect(card).toBeVisible()
  await card.getByRole('button', { name: '不再显示' }).click()
  await expect(card).toHaveCount(0)
  await page.reload()
  await expect(page.getByRole('main').getByRole('heading', { name: '总览', exact: true })).toBeVisible()
  await expect(page.getByRole('region', { name: '快速开始' })).toHaveCount(0)
  expect(await page.evaluate(() => localStorage.getItem('okapi.guide.7'))).toBe('dismissed')

  await page.getByRole('button', { name: '新手引导' }).click()
  await expect(page.getByRole('dialog', { name: '快速开始' })).toBeVisible()
  // 抽屉里的深链换页即关，不盖在目标页上
  await page.getByRole('dialog', { name: '快速开始' }).getByRole('link', { name: '管理密钥' }).click()
  await expect(page).toHaveURL(/\/portal\/keys$/)
  await expect(page.getByRole('dialog', { name: '快速开始' })).toHaveCount(0)
})

test('已有调用记录的用户不见卡片，顶栏入口仍可打开且四步全部完成', async ({ page }) => {
  await prepare(page, { called: true, language: 'en' })
  await page.goto('/portal')
  await expect(page.getByRole('main').getByRole('heading', { name: 'Dashboard', exact: true })).toBeVisible()
  await expect(page.getByRole('region', { name: 'Getting started' })).toHaveCount(0)
  await page.getByRole('button', { name: 'Getting started' }).click()
  const dialog = page.getByRole('dialog', { name: 'Getting started' })
  await expect(dialog.getByText('Keys: 1')).toBeVisible()
  await expect(dialog.getByText('Calls recorded')).toBeVisible()
  await dialog.getByRole('button', { name: 'Finish' }).click()
  await expect(dialog).toHaveCount(0)
  expect(await page.evaluate(() => localStorage.getItem('okapi.guide.7'))).toBe('dismissed')
})

test('密钥页页头可打开接入指南', async ({ page }) => {
  await prepare(page)
  await page.goto('/portal/keys')
  await page.getByRole('button', { name: '接入指南' }).click()
  await expect(page.getByRole('dialog', { name: '快速开始' })).toBeVisible()
  await expect(page.getByRole('dialog', { name: '快速开始' }).getByRole('link', { name: '管理密钥' })).toBeVisible()
})

test('手机端卡片与抽屉不撑宽页面，深色模式可用', async ({ page }) => {
  await prepare(page)
  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto('/portal')
  const card = page.getByRole('region', { name: '快速开始' })
  await expect(card).toBeVisible()
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  await page.screenshot({ path: 'test-results/guide-card-mobile.png', animations: 'disabled' })
  await card.getByRole('button', { name: '打开接入指南' }).click()
  const dialog = page.getByRole('dialog', { name: '快速开始' })
  await dialog.getByRole('group', { name: '客户端' }).getByRole('button', { name: 'Codex CLI' }).click()
  await expect(dialog.getByText('~/.codex/config.toml', { exact: true })).toBeVisible()
  expect(await dialog.evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true)
  await page.evaluate(() => document.documentElement.classList.add('dark'))
  await page.screenshot({ path: 'test-results/guide-drawer-mobile-dark.png', animations: 'disabled' })
})
