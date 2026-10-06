import { expect, test } from '@playwright/test'
import type { Page } from '@playwright/test'
import { fileURLToPath } from 'node:url'

// 出口代理（IMPLEMENTATION §11.41）：代理页、全局默认出口、渠道抽屉与批量设置出口。接口全部打桩。

type Json = Record<string, unknown>

const proxies = [
  {
    id: 1, name: 'hk-1', owner_id: 1, scheme: 'socks5h', host: '10.0.0.1', port: 1080, username: 'alice',
    status: 1, max_keys: 2, max_concurrency: 4, failed_count: 0, cooldown_until: null, cooling: false,
    last_error: null, exit_ip: '203.0.113.7', exit_country: 'HK', latency_ms: 120,
    checked_at: new Date().toISOString(), previous_exit_ip: '203.0.113.6',
    exit_ip_changed_at: new Date().toISOString(),
    note: null, url_masked: 'socks5h://alice:***@10.0.0.1:1080', channel_count: 1, assigned_keys: 1,
    groups: ['hk'], is_default: false,
  },
  {
    id: 2, name: 'jp-1', owner_id: 1, scheme: 'http', host: '10.0.0.2', port: 3128, username: null,
    status: 1, max_keys: null, max_concurrency: null, failed_count: 3, cooldown_until: '2099-01-01T00:00:00Z',
    cooling: true, last_error: 'connect_failed', exit_ip: null, exit_country: null, latency_ms: null,
    checked_at: null, previous_exit_ip: null, exit_ip_changed_at: null,
    note: null, url_masked: 'http://10.0.0.2:3128', channel_count: 0, assigned_keys: 0, groups: ['hk'],
    is_default: false,
  },
]
const groups = [{
  code: 'hk', name: '香港固定', mode: 'pinned', owner_id: 1, description: null,
  members: [
    { proxy_id: 1, name: 'hk-1', status: 1, cooling: false, priority: 0, weight: 1, max_keys: 2, assigned_keys: 1 },
    { proxy_id: 2, name: 'jp-1', status: 1, cooling: true, priority: 0, weight: 1, max_keys: null, assigned_keys: 0 },
  ],
  channel_count: 1, unassigned_keys: 0, is_default: false,
}]
const channel = {
  id: 42, name: 'claude-max-a', provider: 'openai', api_base: 'https://api.openai.com/v1', status: 1,
  priority: 0, models: ['gpt-5'], pools: ['default'],
  pool_members: [{ pool_code: 'default', priority_override: null, weight_override: null }],
  keys: [{ id: 7, status: 1, failed_count: 0, cooldown_until: null, last_error: null, weight: 1,
    max_concurrency: null, credential_kind: 0, egress_proxy_id: null }],
  cost_milli: 1000, data_retention: null, last_test: null, last_balance: null, settings: {},
  egress: { mode: 'proxy', proxy_id: 1 },
}

async function prepare(page: Page, path: string) {
  await page.addInitScript(() => {
    localStorage.setItem('okapi.key', 'egress-ui-fixture')
    localStorage.setItem('okapi.lang', 'zh-CN')
  })
  await page.route('**/*', async (route) => {
    const request = route.request()
    const url = new URL(request.url())
    if (request.isNavigationRequest()) return route.fulfill({
      path: fileURLToPath(new URL('../dist/index.html', import.meta.url)), contentType: 'text/html',
    })
    if (!/^\/(api|admin|auth)\//.test(url.pathname)) return route.continue()
    expect(request.method(), `unmocked write: ${url.pathname}`).toBe('GET')
    const json: Json = url.pathname === '/api/me'
      ? { user_id: 1, key_id: 1, group: 'default', balance_micro: 10_000_000, role: 100, permissions: ['*'] }
      : url.pathname === '/api/notice' ? { notice: null }
        : url.pathname === '/admin/proxies' ? { data: proxies, total: proxies.length }
          : url.pathname === '/admin/proxy-groups' ? { data: groups, total: groups.length }
            : url.pathname === '/admin/egress/default' ? { egress: { mode: 'direct' } }
              : url.pathname === '/admin/channels' ? { data: [channel], total: 1, enabled: 1 }
                : url.pathname === '/admin/pools' ? { data: [], total: 0 }
                  : url.pathname === '/admin/settings/egress_probe_policy' ? { value: null }
                    : url.pathname.startsWith('/admin/settings/') ? { value: null } : { data: [] }
    return route.fulfill({ json })
  })
  await page.goto(path)
}

/// 抽屉打开后焦点才进第一个输入框；在那之前 fill 会被抢焦点打断。
async function openedDialog(page: Page) {
  const dialog = page.getByRole('dialog')
  await expect(dialog.locator(':focus')).toHaveCount(1)
  return dialog
}

test('出口代理页：列表只显示掩码地址与出口事实，熔断中的代理有标记；新建代理提交地址与容量', async ({ page }) => {
  await prepare(page, '/admin/proxies')
  const hk = page.getByRole('row').filter({ hasText: 'hk-1' })
  await expect(hk).toContainText('socks5h://alice:***@10.0.0.1:1080')
  await expect(hk).toContainText('203.0.113.7')
  await expect(hk).toContainText('HK')
  // 出口 IP 七天内变过：标出来，悬停看变化前后
  await expect(hk.getByText('出口 IP 变过')).toHaveAttribute('title', /203\.0\.113\.6 → 203\.0\.113\.7/)
  await expect(hk).toContainText('并发上限 4')
  await expect(page.getByRole('row').filter({ hasText: 'jp-1' })).toContainText('熔断中')
  // 被渠道直接绑定的代理删不掉：按钮直接禁用，而不是等后端 409
  await expect(hk.getByRole('button', { name: '删除', exact: true })).toBeDisabled()

  let created: Json | undefined
  await page.route('**/admin/proxies', async (route) => {
    if (route.request().method() !== 'POST') return route.fallback()
    created = route.request().postDataJSON()
    return route.fulfill({ json: { id: 3, url_masked: 'http://10.0.0.3:3128' } })
  })
  await page.getByRole('button', { name: '添加代理' }).first().click()
  const drawer = await openedDialog(page)
  await drawer.locator('#px-url').fill('ftp://nope')
  await expect(drawer.getByRole('button', { name: '保存', exact: true })).toBeDisabled()
  await drawer.locator('#px-url').fill('http://10.0.0.3:3128')
  await drawer.locator('#px-cap').fill('3')
  await drawer.getByRole('button', { name: '保存', exact: true }).click()
  await expect.poll(() => created).toEqual({ url: 'http://10.0.0.3:3128', max_keys: 3, status: 1 })
})

test('全局默认出口：选代理组后整体 PUT；代理组页签列出成员与分配方式', async ({ page }) => {
  await prepare(page, '/admin/proxies')
  let saved: Json | undefined
  await page.route('**/admin/egress/default', async (route) => {
    if (route.request().method() !== 'PUT') return route.fallback()
    saved = route.request().postDataJSON()
    return route.fulfill({ json: { ok: true, assignment: { assigned: 0, released: 0, unassigned: 2 } } })
  })
  await page.locator('#egress-default-mode').selectOption('group')
  await page.locator('#egress-default-group').selectOption('hk')
  await page.getByRole('button', { name: '保存默认出口' }).click()
  await expect.poll(() => saved).toEqual({ mode: 'group', group_code: 'hk' })
  // 有 key 分不到代理时要提示，别让人以为都分好了
  await expect(page.getByRole('status').filter({ hasText: '2 把 key' })).toBeVisible()

  await page.getByRole('tab', { name: '代理组' }).click()
  const row = page.getByRole('row').filter({ hasText: '香港固定' })
  await expect(row).toContainText('固定分配')
  await expect(row).toContainText('hk-1')
  await expect(row).toContainText('jp-1')
})

test('渠道：列表标出显式出口；编辑抽屉单独保存出口；批量设置出口', async ({ page }) => {
  await prepare(page, '/admin/channels')
  const row = page.getByRole('row').filter({ hasText: 'claude-max-a' })
  await expect(row).toContainText('代理 hk-1')

  let bound: Json | undefined
  await page.route('**/admin/channels/42/egress', async (route) => {
    bound = route.request().postDataJSON()
    return route.fulfill({ json: { ok: true, assignment: { assigned: 1, released: 0, unassigned: 0 } } })
  })
  await row.getByRole('button', { name: '编辑', exact: true }).click()
  const drawer = await openedDialog(page)
  await expect(drawer.locator('#edit-egress-mode')).toHaveValue('proxy')
  await expect(drawer.locator('#edit-egress-proxy')).toHaveValue('1')
  const save = drawer.getByRole('button', { name: '保存出口' })
  await expect(save).toBeDisabled()
  await drawer.locator('#edit-egress-mode').selectOption('group')
  await expect(save).toBeDisabled()
  await drawer.locator('#edit-egress-group').selectOption('hk')
  await save.click()
  await expect.poll(() => bound).toEqual({ mode: 'group', group_code: 'hk' })
  await drawer.getByRole('button', { name: '取消', exact: true }).click()

  let batch: Json | undefined
  await page.route('**/admin/channels/batch', async (route) => {
    batch = route.request().postDataJSON()
    return route.fulfill({ json: { affected: 1, assignment: null } })
  })
  await row.getByRole('checkbox').check()
  await page.getByRole('button', { name: '设置出口' }).click()
  const batchDrawer = page.getByRole('dialog')
  await batchDrawer.locator('#batch-egress-mode').selectOption('direct')
  await batchDrawer.getByRole('button', { name: '保存', exact: true }).click()
  await expect.poll(() => batch).toEqual({ ids: [42], action: 'set_egress', egress: { mode: 'direct' } })
})

test('批量导入：按行提交、带上缺省协议与统一设置，逐行回显跳过原因', async ({ page }) => {
  await prepare(page, '/admin/proxies')
  let imported: Json | undefined
  await page.route('**/admin/proxies/import', async (route) => {
    imported = route.request().postDataJSON()
    return route.fulfill({ json: {
      created: [{ line: 1, id: 9, name: 'hk-1', url_masked: 'http://1.2.3.4:8080' }],
      skipped: [{ line: 2, reason: 'duplicate' }, { line: 3, reason: 'invalid' }],
      assignment: { assigned: 1, released: 0, unassigned: 0 },
    } })
  })
  await page.getByRole('button', { name: '批量导入' }).click()
  const drawer = await openedDialog(page)
  const submit = drawer.getByRole('button', { name: /导入 \d+ 行/ })
  await expect(submit).toBeDisabled()
  await drawer.locator('#px-import').fill('1.2.3.4:8080\n# 注释\n\n1.2.3.4:8080\nnope')
  await expect(submit).toHaveText('导入 3 行')
  await drawer.locator('#px-import-scheme').selectOption('http')
  await drawer.locator('#px-import-prefix').fill('hk')
  await drawer.locator('#px-import-cap').fill('2')
  await drawer.locator('#px-import-group').selectOption('hk')
  await submit.click()
  await expect.poll(() => imported).toEqual({
    text: '1.2.3.4:8080\n# 注释\n\n1.2.3.4:8080\nnope', default_scheme: 'http', name_prefix: 'hk',
    max_keys: 2, group_code: 'hk',
  })
  await expect(drawer.getByRole('status')).toContainText('新建 1 个')
  await expect(drawer).toContainText('第 2 行：已存在相同地址')
  await expect(drawer).toContainText('第 3 行：格式认不出')
})

test('后台探测：缺省值回显；改间隔与探测地址后整体保存，关掉开关立即生效', async ({ page }) => {
  await prepare(page, '/admin/proxies')
  const posts: Json[] = []
  await page.route('**/admin/settings', async (route) => {
    if (route.request().method() !== 'POST') return route.fallback()
    posts.push(route.request().postDataJSON())
    return route.fulfill({ json: { ok: true } })
  })
  await expect(page.locator('#egress-probe-interval')).toHaveValue('10')
  await page.locator('#egress-probe-interval').fill('0')
  await expect(page.getByText('请求参数有误（interval_secs）')).toBeVisible()
  await page.locator('#egress-probe-interval').fill('30')
  await page.locator('#egress-probe-target').fill('https://ip.example.com/json')
  await page.locator('#egress-probe-target').locator('xpath=ancestor::div[contains(@class,"flex-wrap")][1]')
    .getByRole('button', { name: '保存', exact: true }).click()
  await expect.poll(() => posts[0]).toEqual({ key: 'egress_probe_policy', value: {
    enabled: true, interval_secs: 1800, target: 'https://ip.example.com/json', concurrency: 4,
  } })
  await page.getByRole('switch', { name: '开启后台探测' }).click()
  await expect.poll(() => posts.length).toBe(2)
  expect((posts[1].value as Json).enabled).toBe(false)
})
