import { expect, test } from '@playwright/test'
import type { Page } from '@playwright/test'
import { fileURLToPath } from 'node:url'

// 写操作表单回归（接口桩，不碰数据库）：渠道抽屉的注入字段 / 出站代理 / 额外请求头、
// 门户安全页的会话吊销卡、找回与重置密码页。断言的重点是提交给后端的请求体形状——
// 这些字段后端有写校验用例，但此前没有任何用例证明前端真的把它们按约定形状送出去。

type Json = Record<string, unknown>

/// 通用桩：导航一律回 SPA 壳；未被更具体路由接住的接口只允许 GET，回空列表。
/// 写请求必须由各用例自己注册的路由承接，否则这里会把它当成"意外写入"直接报红。
async function prepare(page: Page, { permissions = ['*'], signedIn = true } = {}) {
  const requests: string[] = []
  await page.addInitScript(
    ({ signedIn }) => {
      if (signedIn) localStorage.setItem('okapi.key', 'interaction-test-key')
      else localStorage.removeItem('okapi.key')
      localStorage.setItem('okapi.lang', 'zh-CN')
    },
    { signedIn },
  )
  await page.route('**/*', async (route) => {
    const request = route.request()
    const path = new URL(request.url()).pathname
    if (request.isNavigationRequest()) {
      await route.fulfill({
        path: fileURLToPath(new URL('../dist/index.html', import.meta.url)),
        contentType: 'text/html',
      })
    } else if (/^\/(api|admin|auth|pay)\//.test(path)) {
      requests.push(`${request.method()} ${path}`)
      expect(request.method(), `未被桩接住的写请求：${path}`).toBe('GET')
      const json: Json =
        path === '/api/me'
          ? {
              user_id: 1,
              key_id: 1,
              group: 'default',
              balance_micro: 10_000_000,
              balance_expires_at: null,
              role: permissions.length ? 100 : 1,
              permissions,
            }
          : path === '/api/notice'
            ? { notice: null }
            : path.startsWith('/admin/settings/')
              ? { value: null }
              : { data: [], next_before: null }
      await route.fulfill({ json })
    } else {
      await route.continue()
    }
  })
  return requests
}

function apiError(status: number, code: string, param?: string) {
  return { status, json: { error: { code, ...(param === undefined ? {} : { param }) } } }
}

const CHANNEL = {
  id: 42,
  name: 'openai-main',
  provider: 'openai',
  api_base: 'https://api.openai.com/v1',
  status: 1,
  priority: 0,
  models: ['gpt-5'],
  keys: [],
  settings: {
    strip_request_fields: ['logit_bias'],
    proxy_url: 'socks5://127.0.0.1:1080',
    extra_headers: { 'OpenAI-Organization': 'org-1' },
  },
  pools: ['default'],
  pool_members: [{ pool_code: 'default', priority_override: null, weight_override: null }],
  cost_milli: 1000,
  data_retention: null,
  last_test: null,
}

test('渠道抽屉：注入字段按 JSON 解析、代理与额外头可改可清，保存体只含有值的键；受保护键的 400 以错误码文案提示', async ({ page }) => {
  await prepare(page)
  const patches: Json[] = []
  await page.route('**/admin/channels?*', (route) =>
    route.fulfill({ json: { data: [CHANNEL], total: 1, enabled: 1 } }),
  )
  await page.route('**/admin/channels/42', async (route) => {
    const body = route.request().postDataJSON() as { settings: Json }
    patches.push(body)
    const inject = body.settings.inject_request_fields as Json | undefined
    if (inject && 'model' in inject) {
      await route.fulfill(apiError(400, 'bad_request', 'inject_request_fields'))
    } else {
      await route.fulfill({ json: { ok: true } })
    }
  })

  await page.goto('/admin/channels')
  await page
    .getByRole('row')
    .filter({ hasText: 'openai-main' })
    .getByRole('button', { name: '编辑', exact: true })
    .click()
  const drawer = page.getByRole('dialog')
  await expect(drawer.getByRole('heading', { name: /编辑渠道/ })).toBeVisible()
  await drawer.getByRole('tab', { name: '请求与计费行为' }).click()

  // 已有配置原样回显
  const proxy = drawer.locator('#d-proxy')
  await expect(proxy).toHaveValue('socks5://127.0.0.1:1080')
  const headerName = drawer.getByPlaceholder('OpenAI-Organization')
  await expect(headerName).toHaveValue('OpenAI-Organization')
  await expect(drawer.getByPlaceholder('org-…')).toHaveValue('org-1')

  // 注入两行：数字按 JSON 解析成 number，带引号的字面量解析成 string
  await drawer.getByRole('button', { name: '添加字段' }).click()
  await drawer.getByPlaceholder('temperature').nth(0).fill('temperature')
  await drawer.getByPlaceholder('0.2 or "forced"').nth(0).fill('0.2')
  await drawer.getByRole('button', { name: '添加字段' }).click()
  await drawer.getByPlaceholder('temperature').nth(1).fill('user')
  await drawer.getByPlaceholder('0.2 or "forced"').nth(1).fill('"forced"')

  await proxy.fill('http://proxy.internal:3128')
  // 删掉唯一一条额外头：对象为空时键应从 settings 里消失，而不是送一个 {}
  await headerName.locator('..').getByRole('button', { name: '×' }).click()

  await drawer.getByRole('button', { name: '保存', exact: true }).click()
  await expect(page.getByRole('status').filter({ hasText: '操作成功' })).toBeVisible()
  expect(patches).toHaveLength(1)
  expect(patches[0].settings).toEqual({
    thinking_to_content: false,
    bill_by_response_model: false,
    strip_request_fields: ['logit_bias'],
    inject_request_fields: { temperature: 0.2, user: 'forced' },
    proxy_url: 'http://proxy.internal:3128',
  })
  expect(patches[0]).toMatchObject({ name: 'openai-main', models: ['gpt-5'], priority: 0 })

  // 受保护键交给后端拒绝：错误码 + param 渲染成可读文案，抽屉保持打开可改
  await drawer.getByRole('button', { name: '添加字段' }).click()
  await drawer.getByPlaceholder('temperature').nth(2).fill('model')
  await drawer.getByPlaceholder('0.2 or "forced"').nth(2).fill('hijack')
  await drawer.getByRole('button', { name: '保存', exact: true }).click()
  await expect(page.getByRole('alert')).toContainText('inject_request_fields')
  await expect(drawer).toBeVisible()
  expect(patches).toHaveLength(2)
  expect((patches[1].settings as Json).inject_request_fields).toEqual({
    temperature: 0.2,
    user: 'forced',
    model: 'hijack',
  })
})

test('安全页会话卡：列出会话并标出当前浏览器，单条吊销与全部吊销各打对应端点，空态文案随之出现', async ({ page }) => {
  await prepare(page, { permissions: [] })
  let sessions = [
    { sid: 'sid-current', ip: '203.0.113.7', ua: 'Mozilla/5.0 Chrome/128', created_at: 1_757_000_000, current: true },
    { sid: 'sid-phone', ip: '198.51.100.9', ua: 'okapi-ios/1.0', created_at: 1_756_900_000, current: false },
  ]
  const deletes: string[] = []
  await page.route(/\/api\/me\/sessions(\/[^/?]+)?$/, async (route) => {
    const request = route.request()
    const path = new URL(request.url()).pathname
    if (request.method() === 'GET') {
      await route.fulfill({ json: { data: sessions } })
      return
    }
    expect(request.method()).toBe('DELETE')
    deletes.push(path)
    if (path === '/api/me/sessions') {
      sessions = []
    } else {
      const sid = decodeURIComponent(path.slice('/api/me/sessions/'.length))
      sessions = sessions.filter((s) => s.sid !== sid)
    }
    await route.fulfill({ json: { ok: true } })
  })

  await page.goto('/portal/security')
  // Card > CardHeader > h3：从标题上溯两层拿到卡片根，空态与列表态都稳定
  const card = page.getByRole('heading', { name: '有效登录会话' }).locator('xpath=../..')
  await expect(card.getByRole('listitem')).toHaveCount(2)
  const current = card.getByRole('listitem').filter({ hasText: '203.0.113.7' })
  await expect(current.getByText('当前浏览器')).toBeVisible()
  await expect(card.getByRole('listitem').filter({ hasText: '198.51.100.9' }).getByText('当前浏览器')).toHaveCount(0)

  await card.getByRole('listitem').filter({ hasText: '198.51.100.9' }).getByRole('button', { name: '吊销', exact: true }).click()
  await expect(page.getByRole('status').filter({ hasText: '已吊销该会话' })).toBeVisible()
  await expect(card.getByRole('listitem')).toHaveCount(1)
  expect(deletes).toEqual(['/api/me/sessions/sid-phone'])

  await card.getByRole('button', { name: '吊销全部会话' }).click()
  await expect(page.getByRole('status').filter({ hasText: '已吊销全部 web 会话' })).toBeVisible()
  await expect(card.getByText(/没有有效的 web 会话/)).toBeVisible()
  await expect(card.getByRole('button', { name: '吊销全部会话' })).toHaveCount(0)
  expect(deletes).toEqual(['/api/me/sessions/sid-phone', '/api/me/sessions'])
})

test('找回密码：登录页链接带上已填邮箱，提交体含邮箱与语言，成功态不暴露账号存在性，未配 SMTP 给站长向文案', async ({ page }) => {
  await prepare(page, { signedIn: false })
  const posts: Json[] = []
  let smtpConfigured = true
  await page.route('**/auth/password/forgot', async (route) => {
    posts.push(route.request().postDataJSON() as Json)
    if (smtpConfigured) await route.fulfill({ json: { ok: true } })
    else await route.fulfill(apiError(501, 'smtp_not_configured'))
  })

  await page.goto('/')
  await page.getByRole('button', { name: /邮箱登录/ }).click()
  await page.locator('#email').fill('who@ok.test')
  await page.getByRole('link', { name: '忘记密码？' }).click()
  await expect(page).toHaveURL(/\/forgot-password\?email=who(%40|@)ok\.test$/)
  const email = page.locator('#forgot-email')
  await expect(email).toHaveValue('who@ok.test')

  const send = page.getByRole('button', { name: '发送重置链接' })
  await email.fill('not-an-email')
  await expect(send).toBeDisabled()
  await email.fill('who@ok.test')
  await send.click()
  const sent = page.getByRole('status').filter({ hasText: 'who@ok.test' })
  await expect(sent).toBeVisible()
  await expect(sent).toContainText('30 分钟')
  expect(posts).toEqual([{ email: 'who@ok.test', lang: 'zh-CN' }])

  smtpConfigured = false
  await page.goto('/forgot-password')
  await page.locator('#forgot-email').fill('who@ok.test')
  await page.getByRole('button', { name: '发送重置链接' }).click()
  await expect(page.getByRole('alert')).toContainText('尚未配置邮件服务')
  expect(posts).toHaveLength(2)
})

test('用户抽屉：入账按 USD 输入换成 micro 整数，系数按字符串提交并前置校验，分组全量按顺序定优先级，封禁经确认框', async ({ page }) => {
  await prepare(page)
  const posts: { path: string; body: Json }[] = []
  let status = 1
  const overview = () => ({
    user: { id: 7, username: 'alice', role: 1, status, balance_micro: 5_000_000, price_multiplier: '1' },
    groups: [{ code: 'default', priority: 1 }],
    keys: [],
  })
  await page.route('**/admin/users?*', (route) =>
    route.fulfill({
      json: {
        total: 1,
        data: [{ id: 7, username: 'alice', email: 'alice@ok.test', role: 1, status: 1, balance_micro: 5_000_000, admin_role_id: null, price_multiplier: '1' }],
      },
    }),
  )
  await page.route('**/admin/users/7/overview', (route) => route.fulfill({ json: overview() }))
  await page.route('**/admin/users/7/usage?*', (route) =>
    route.fulfill({ json: { days: 7, stats_available: false, daily: [], by_model: [], ledger: [] } }),
  )
  await page.route(/\/admin\/users\/7\/(credit|multiplier|groups|manage)$/, async (route) => {
    const path = new URL(route.request().url()).pathname
    const body = route.request().postDataJSON() as Json
    posts.push({ path, body })
    if (path.endsWith('/credit')) {
      await route.fulfill({ json: { balance_after_micro: 5_000_000 + Number(body.amount_micro) } })
      return
    }
    if (path.endsWith('/manage') && body.action === 'ban') status = 2
    await route.fulfill({ json: { ok: true } })
  })

  await page.goto('/admin/users')
  await page.getByRole('row').filter({ hasText: 'alice' }).getByRole('button', { name: '管理', exact: true }).click()
  const drawer = page.getByRole('dialog')
  await expect(drawer.getByRole('heading', { name: '用户 #7' })).toBeVisible()

  // 余额：界面填 USD，提交 micro 整数；浮点边界值也必须落到整数
  await drawer.getByRole('tab', { name: '余额', exact: true }).click()
  const credit = drawer.getByRole('button', { name: '入账', exact: true })
  await expect(credit).toBeDisabled()
  await drawer.locator('#amt').fill('12.34')
  await drawer.locator('#reason').fill('refund ticket 42')
  await credit.click()
  await expect(page.getByRole('status').filter({ hasText: '入账后余额' })).toBeVisible()
  await expect(drawer.locator('#amt')).toHaveValue('')
  await drawer.locator('#amt').fill('0.29')
  await credit.click()
  await expect(page.getByRole('status').filter({ hasText: '入账后余额' })).toHaveCount(2)
  expect(posts.filter((p) => p.path.endsWith('/credit')).map((p) => p.body)).toEqual([
    { amount_micro: 12_340_000, reason: 'refund ticket 42' },
    { amount_micro: 290_000, reason: 'refund ticket 42' },
  ])

  // 个人系数：十进制字符串原样提交（计费链路不吃浮点）；负数 / 未改动不放行
  const multiplier = drawer.locator('#multiplier')
  const saveMultiplier = drawer.getByRole('button', { name: '保存', exact: true })
  await expect(saveMultiplier).toBeDisabled()
  await multiplier.fill('-1')
  await expect(saveMultiplier).toBeDisabled()
  await multiplier.fill('0.8')
  await saveMultiplier.click()
  await expect(page.getByRole('status').filter({ hasText: '操作成功' })).toBeVisible()
  expect(posts.find((p) => p.path.endsWith('/multiplier'))?.body).toEqual({ multiplier: '0.8' })

  // 分组：全量覆盖，先出现的优先级高
  await drawer.getByRole('tab', { name: '分组', exact: true }).click()
  const tagInput = drawer.getByPlaceholder('回车或逗号分隔，可粘贴多个')
  await tagInput.fill('vip')
  await tagInput.press('Enter')
  const groupsSaved = page.waitForRequest((r) => r.method() === 'POST' && r.url().endsWith('/groups'))
  await drawer.getByRole('button', { name: '保存', exact: true }).click()
  await groupsSaved
  expect(posts.find((p) => p.path.endsWith('/groups'))?.body).toEqual({
    groups: [
      { group_code: 'default', priority: 2 },
      { group_code: 'vip', priority: 1 },
    ],
  })

  // 封禁：会吊销全部令牌，必须过确认框；成功后按钮翻成解封
  await drawer.getByRole('tab', { name: '用户动作', exact: true }).click()
  await drawer.getByRole('button', { name: '封禁', exact: true }).click()
  expect(posts.some((p) => p.path.endsWith('/manage'))).toBe(false)
  const confirm = page.getByRole('alertdialog')
  await expect(confirm).toBeVisible()
  await confirm.getByRole('button', { name: '封禁', exact: true }).click()
  await expect(drawer.getByRole('button', { name: '解封', exact: true })).toBeVisible()
  expect(posts.filter((p) => p.path.endsWith('/manage')).map((p) => p.body)).toEqual([{ action: 'ban' }])
})

test('模型定价抽屉：倍率轴按十进制字符串提交，空档位行被过滤，阶梯表达式与降级链显式回传', async ({ page }) => {
  await prepare(page)
  const posts: Json[] = []
  const gpt5 = {
    model_name: 'gpt-5', vendor: 'OpenAI', status: 1, pricing_mode: 'ratio',
    model_ratio: '1.25', completion_ratio: '8', cache_ratio: '0.1', cache_write_ratio: null, tier_expr: null,
    audio_ratio: null, audio_completion_ratio: null, image_ratio: null, per_call_price_micro: null,
    fallback_models: ['gpt-4o-mini'],
  }
  await page.route('**/admin/models?*', (route) => route.fulfill({ json: { data: [gpt5], total: 1, unpriced: 0 } }))
  await page.route('**/admin/models', async (route) => {
    if (route.request().method() !== 'POST') return route.fallback()
    posts.push(route.request().postDataJSON() as Json)
    await route.fulfill({ json: { ok: true } })
  })

  await page.goto('/admin/pricing')
  await page.getByRole('row').filter({ hasText: 'gpt-5' }).getByRole('button', { name: '编辑', exact: true }).click()
  const drawer = page.getByRole('dialog')
  await expect(drawer.getByRole('heading', { name: '编辑 gpt-5' })).toBeVisible()
  await expect(drawer.locator('#m-name')).toHaveValue('gpt-5')
  await expect(drawer.locator('#m-name')).toHaveAttribute('readonly', '')
  await expect(drawer.locator('#ax-model_ratio')).toHaveValue('1.25')
  await expect(drawer.locator('#ax-cache_write_ratio')).toHaveValue('1')

  await drawer.locator('#ax-model_ratio').fill('1.5')
  await drawer.locator('#ax-cache_ratio').fill('0.125')
  await drawer.getByRole('button', { name: '添加档位' }).click()
  await drawer.locator('#tier-0').fill('flex')
  await drawer.locator('#tratio-0').fill('0.5')
  // 第二行只加不填：空档位名不该变成 "" 键
  await drawer.getByRole('button', { name: '添加档位' }).click()
  const saved = page.waitForRequest((r) => r.method() === 'POST' && r.url().endsWith('/admin/models'))
  await drawer.getByRole('button', { name: '保存', exact: true }).click()
  await saved
  await expect(drawer).toBeHidden()
  expect(posts).toEqual([
    {
      model_name: 'gpt-5',
      model_ratio: '1.5',
      completion_ratio: '8',
      cache_ratio: '0.125',
      cache_write_ratio: '1',
      audio_ratio: '1',
      audio_completion_ratio: '1',
      image_ratio: '1',
      tier_ratios: { flex: '0.5' },
      fallback_models: ['gpt-4o-mini'],
      tier_expr: '',
    },
  ])

  // 新建 + 阶梯表：模式提示随表达式切换；无档位时不发 tier_ratios 键
  await page.getByRole('button', { name: '新建模型' }).click()
  const create = page.getByRole('dialog')
  await create.locator('#m-name').fill('my-model')
  await expect(create.getByText('当前：ratio 模式', { exact: false })).toBeVisible()
  await create.locator('#m-tier-expr').fill(' 0:2.5,128000:5 ')
  await expect(create.getByText('当前：tiered 模式', { exact: false })).toBeVisible()
  const created = page.waitForRequest((r) => r.method() === 'POST' && r.url().endsWith('/admin/models'))
  await create.getByRole('button', { name: '保存', exact: true }).click()
  await created
  await expect(create).toBeHidden()
  expect(posts).toHaveLength(2)
  expect(posts[1]).toEqual({
    model_name: 'my-model',
    model_ratio: '1',
    completion_ratio: '1',
    cache_ratio: '1',
    cache_write_ratio: '1',
    audio_ratio: '1',
    audio_completion_ratio: '1',
    image_ratio: '1',
    fallback_models: [],
    tier_expr: '0:2.5,128000:5',
  })
  expect('tier_ratios' in posts[1]).toBe(false)
})

test('套餐抽屉：充值模板与订阅两种形态字段互斥，USD 换 micro、天数取整、空值不发键，订阅缺有效期不放行', async ({ page }) => {
  await prepare(page)
  const posts: Json[] = []
  const plans: Json[] = [{
    id: 1, plan_code: 'starter', display_name: 'Starter', kind: 0, grant_micro: 10_000_000, group_code: 'vip',
    balance_valid_days: 90, price_micro: 0, period: null, duration_days: null, sort_order: 5, description: null,
    status: 1, code_count: 3, active_subscribers: 0,
  }]
  await page.route('**/admin/plans?*', (route) => route.fulfill({ json: { data: plans, total: plans.length } }))
  await page.route('**/admin/plans', async (route) => {
    if (route.request().method() !== 'POST') return route.fallback()
    posts.push(route.request().postDataJSON() as Json)
    await route.fulfill({ json: { plan_id: 9 } })
  })
  await page.route('**/admin/groups', (route) =>
    route.fulfill({ json: { data: [{ group_code: 'default' }, { group_code: 'vip' }] } }),
  )

  // 编辑既有充值模板：代码锁定、金额回填为 USD，改有效期与分组后提交
  await page.goto('/admin/plans')
  await page.getByRole('row').filter({ hasText: 'starter' }).getByRole('button', { name: '编辑', exact: true }).click()
  const edit = page.getByRole('dialog')
  await expect(edit.getByRole('heading', { name: '编辑套餐 starter' })).toBeVisible()
  await expect(edit.locator('#p-code')).toBeDisabled()
  await expect(edit.locator('#p-grant')).toHaveValue('10')
  await expect(edit.locator('#p-days')).toHaveValue('90')
  await edit.locator('#p-grant').fill('12.5')
  await edit.locator('#p-days').fill('30.9')
  await edit.locator('#p-group').selectOption('default')
  let saved = page.waitForRequest((r) => r.method() === 'POST' && r.url().endsWith('/admin/plans'))
  await edit.getByRole('button', { name: '保存', exact: true }).click()
  await saved
  await expect(edit).toBeHidden()
  expect(posts[0]).toEqual({
    plan_code: 'starter',
    display_name: 'Starter',
    kind: 0,
    grant_micro: 12_500_000,
    group_code: 'default',
    balance_valid_days: 30,
    price_micro: 0,
    sort_order: 5,
  })
  expect('period' in posts[0] || 'duration_days' in posts[0] || 'description' in posts[0]).toBe(false)

  // 新建订阅：切到订阅形态后字段区替换；有效期为空不放行；售价空 = 不售卖（0）
  await page.getByRole('button', { name: '新建套餐' }).click()
  const create = page.getByRole('dialog')
  await create.locator('#p-code').fill('pro-monthly')
  await create.locator('#p-name').fill('Pro')
  await create.getByRole('group', { name: '类型', exact: true }).getByRole('button', { name: '订阅', exact: true }).click()
  await expect(create.locator('#p-grant')).toHaveCount(0)
  await expect(create.locator('#p-quota')).toHaveValue('10')
  const save = create.getByRole('button', { name: '保存', exact: true })
  await create.locator('#p-duration').fill('')
  await expect(create.getByText('订阅必须填写有效期天数。')).toBeVisible()
  await expect(save).toBeDisabled()
  await create.locator('#p-duration').fill('30')
  await create.locator('#p-quota').fill('20')
  await create.getByRole('group', { name: '周期', exact: true }).getByRole('button', { name: '每周', exact: true }).click()
  await expect(create.getByText(/每周重置 .*额度，有效 30 天/)).toBeVisible()
  await create.locator('#p-price').fill('9.99')
  await create.locator('#p-desc').fill('  best value  ')
  await expect(save).toBeEnabled()
  saved = page.waitForRequest((r) => r.method() === 'POST' && r.url().endsWith('/admin/plans'))
  await save.click()
  await saved
  expect(posts[1]).toEqual({
    plan_code: 'pro-monthly',
    display_name: 'Pro',
    kind: 1,
    grant_micro: 20_000_000,
    price_micro: 9_990_000,
    period: 2,
    duration_days: 30,
    sort_order: 0,
    description: 'best value',
  })
  expect('balance_valid_days' in posts[1] || 'group_code' in posts[1]).toBe(false)
})

test('角色抽屉：权限点来自后端清单、整组切换、无权限不放行、按 code 提交；删除经确认框且 409 以错误码文案提示', async ({ page }) => {
  await prepare(page)
  const posts: Json[] = []
  const deletes: string[] = []
  await page.route('**/admin/permissions', (route) =>
    route.fulfill({ json: { data: ['billing.read', 'billing.refund', 'channel.read', 'channel.write', 'user.read'] } }),
  )
  await page.route('**/admin/roles?*', (route) =>
    route.fulfill({
      json: { data: [{ id: 3, role_code: 'ops_readonly', display_name: 'Ops', permissions: ['billing.read'] }], total: 1 },
    }),
  )
  await page.route('**/admin/roles', async (route) => {
    if (route.request().method() !== 'POST') return route.fallback()
    posts.push(route.request().postDataJSON() as Json)
    await route.fulfill({ json: { role_id: 4 } })
  })
  await page.route('**/admin/roles/*', async (route) => {
    if (route.request().method() !== 'DELETE') return route.fallback()
    deletes.push(new URL(route.request().url()).pathname)
    await route.fulfill(apiError(409, 'role_in_use'))
  })

  await page.goto('/admin/roles')
  await page.getByRole('button', { name: '新建角色' }).click()
  const drawer = page.getByRole('dialog')
  const create = drawer.getByRole('button', { name: '新建', exact: true })
  await drawer.locator('#rcode').fill('finance')
  await drawer.locator('#rname').fill('Finance')
  await expect(create).toBeDisabled()
  await expect(drawer.getByText('已选 0 个权限点')).toBeVisible()

  // 整组切换 billing 两项，再单勾一项，再取消其中一项
  // 域名 span 的父节点就是该组的标题行（span + 整组切换按钮）
  const billingHeader = drawer.getByText('billing', { exact: true }).locator('..')
  await billingHeader.getByRole('button', { name: '整组切换' }).click()
  await expect(drawer.getByText('已选 2 个权限点')).toBeVisible()
  await drawer.getByRole('checkbox', { name: 'channel.read', exact: true }).check()
  await drawer.getByRole('checkbox', { name: 'billing.refund', exact: true }).uncheck()
  await expect(drawer.getByText('已选 2 个权限点')).toBeVisible()
  await expect(create).toBeEnabled()
  const posted = page.waitForRequest((r) => r.method() === 'POST' && r.url().endsWith('/admin/roles'))
  await create.click()
  await posted
  await expect(drawer).toBeHidden()
  expect(posts).toHaveLength(1)
  expect(posts[0]).toMatchObject({ role_code: 'finance', display_name: 'Finance' })
  expect([...(posts[0].permissions as string[])].sort()).toEqual(['billing.read', 'channel.read'])

  // 编辑既有角色：code 锁定、已有权限预勾
  await page.getByRole('row').filter({ hasText: 'ops_readonly' }).getByRole('button', { name: '编辑', exact: true }).click()
  const edit = page.getByRole('dialog')
  await expect(edit.getByRole('heading', { name: '编辑角色 ops_readonly' })).toBeVisible()
  await expect(edit.locator('#rcode')).toBeDisabled()
  await expect(edit.getByRole('checkbox', { name: 'billing.read', exact: true })).toBeChecked()
  await edit.getByRole('button', { name: '取消', exact: true }).click()

  // 删除：确认框 → DELETE /admin/roles/{code}；后端 409 role_in_use 渲染成可读文案
  await page.getByRole('row').filter({ hasText: 'ops_readonly' }).getByRole('button', { name: '删除', exact: true }).click()
  const confirm = page.getByRole('alertdialog')
  await expect(confirm).toContainText('删除 ops_readonly？')
  expect(deletes).toHaveLength(0)
  await confirm.getByRole('button', { name: '删除', exact: true }).click()
  await expect(page.getByRole('alert')).toContainText('仍有用户绑定')
  expect(deletes).toEqual(['/admin/roles/ops_readonly'])
})

test('价格分组抽屉：倍率按字符串提交、池从后端清单选、自选开关落体；编辑态分组码只读；内置默认组不可删', async ({ page }) => {
  await prepare(page)
  const posts: Json[] = []
  const groups = [
    { group_code: 'default', group_ratio: '1', description: null, is_default: true, user_count: 12, channel_count: 3, pool_code: 'default', self_select: false },
    { group_code: 'vip', group_ratio: '0.8', description: 'VIP', is_default: false, user_count: 2, channel_count: 1, pool_code: 'premium', self_select: true },
  ]
  await page.route('**/admin/groups?*', (route) => route.fulfill({ json: { data: groups, total: groups.length } }))
  await page.route('**/admin/groups', async (route) => {
    if (route.request().method() !== 'POST') return route.fallback()
    posts.push(route.request().postDataJSON() as Json)
    await route.fulfill({ json: { ok: true } })
  })
  await page.route('**/admin/pools', (route) =>
    route.fulfill({ json: { data: [{ pool_code: 'default' }, { pool_code: 'premium' }] } }),
  )
  // 抽屉里"选了池就地看可达"的详情：members / models / groups 三个数组必须在
  await page.route('**/admin/pools/*', (route) => {
    const code = decodeURIComponent(new URL(route.request().url()).pathname.split('/').pop() ?? '')
    return route.fulfill({
      json: {
        pool_code: code, description: null, routing_strategy: 'weighted', fallback_pool_code: null,
        members: [{ channel_id: 1, name: 'openai-main', provider: 'openai', status: 1, priority: 0, priority_override: null, weight_override: null, models: ['gpt-5'], active_keys: 1 }],
        models: ['gpt-5'], groups: [code === 'premium' ? 'vip' : 'default'],
      },
    })
  })

  await page.goto('/admin/groups')
  // 内置默认组的删除按钮禁用：删掉它等于把没配分组的用户全体断供
  await expect(
    page.getByRole('row').filter({ hasText: 'default' }).first().getByRole('button', { name: '删除', exact: true }),
  ).toBeDisabled()

  await page.getByRole('row').filter({ hasText: 'vip' }).getByRole('button', { name: '编辑', exact: true }).click()
  const edit = page.getByRole('dialog')
  await expect(edit.getByRole('heading', { name: '编辑 vip' })).toBeVisible()
  await expect(edit.locator('#g-code')).toHaveAttribute('readonly', '')
  await expect(edit.locator('#g-ratio')).toHaveValue('0.8')
  await expect(edit.locator('#g-pool')).toHaveValue('premium')
  await expect(edit.getByRole('switch', { name: '允许用户自选此分组' })).toBeChecked()
  await edit.locator('#g-ratio').fill(' 0.75 ')
  await edit.locator('#g-desc').fill(' VIP tier ')
  await edit.locator('#g-pool').selectOption('default')
  await edit.getByRole('switch', { name: '允许用户自选此分组' }).click()
  const saved = page.waitForRequest((r) => r.method() === 'POST' && r.url().endsWith('/admin/groups'))
  await edit.getByRole('button', { name: '保存', exact: true }).click()
  await saved
  await expect(edit).toBeHidden()
  expect(posts).toEqual([
    { group_code: 'vip', group_ratio: '0.75', description: 'VIP tier', pool_code: 'default', self_select: false },
  ])

  // 新建：分组码为空不放行；缺省倍率 1、缺省池 default、不可自选
  await page.getByRole('button', { name: '新建分组' }).click()
  const create = page.getByRole('dialog')
  const save = create.getByRole('button', { name: '保存', exact: true })
  await expect(save).toBeDisabled()
  await create.locator('#g-code').fill('team-a')
  await expect(create.locator('#g-ratio')).toHaveValue('1')
  await expect(create.locator('#g-pool')).toHaveValue('default')
  const created = page.waitForRequest((r) => r.method() === 'POST' && r.url().endsWith('/admin/groups'))
  await save.click()
  await created
  expect(posts[1]).toEqual({ group_code: 'team-a', group_ratio: '1', description: '', pool_code: 'default', self_select: false })
})

test('计费规则抽屉：按类型只发该类型字段，阈值 USD 换 micro，空范围不发键，时段星期勾选排序；列表上下线与删除提示需发布', async ({ page }) => {
  await prepare(page)
  const posts: Json[] = []
  const toggles: { path: string; body: Json }[] = []
  const deletes: string[] = []
  const rules = [{
    rule_code: 'night-discount', rule_type: 'time_based', priority: 5, enabled: true, valid_from: null, valid_to: null,
    scope: { groups: ['vip'] },
    params: { multiplier: '0.5', start_minute: 0, end_minute: 359, weekdays: [1, 5], stacking_mode: 'exclusive' },
  }]
  await page.route('**/admin/pricing/rules?*', (route) => route.fulfill({ json: { data: rules, total: 1 } }))
  await page.route('**/admin/pricing/rules', async (route) => {
    if (route.request().method() !== 'POST') return route.fallback()
    posts.push(route.request().postDataJSON() as Json)
    await route.fulfill({ json: { ok: true } })
  })
  await page.route(/\/admin\/pricing\/rules\/[^/?]+(\/toggle)?$/, async (route) => {
    const request = route.request()
    const path = new URL(request.url()).pathname
    if (request.method() === 'POST') toggles.push({ path, body: request.postDataJSON() as Json })
    else if (request.method() === 'DELETE') deletes.push(path)
    else return route.fallback()
    await route.fulfill({ json: { ok: true } })
  })

  await page.goto('/admin/rules')
  // 编辑既有时段规则：类型 / 参数 / 星期 / 范围全部回填，code 锁定
  await page.getByRole('row').filter({ hasText: 'night-discount' }).getByRole('button', { name: '编辑', exact: true }).click()
  const edit = page.getByRole('dialog')
  await expect(edit.getByRole('heading', { name: '编辑规则 night-discount' })).toBeVisible()
  await expect(edit.locator('#r-code')).toBeDisabled()
  await expect(edit.locator('#r-type')).toHaveValue('time_based')
  await expect(edit.locator('#r-mult')).toHaveValue('0.5')
  await expect(edit.locator('#r-start')).toHaveValue('0')
  await expect(edit.locator('#r-end')).toHaveValue('359')
  await expect(edit.getByRole('checkbox', { name: '一', exact: true })).toBeChecked()
  await expect(edit.getByRole('checkbox', { name: '五', exact: true })).toBeChecked()
  await expect(edit.locator('#r-stacking')).toHaveValue('exclusive')
  // 再勾周六：提交时按升序
  await edit.getByRole('checkbox', { name: '六', exact: true }).check()
  await edit.locator('#r-end').fill('420')
  let saved = page.waitForRequest((r) => r.method() === 'POST' && r.url().endsWith('/admin/pricing/rules'))
  await edit.getByRole('button', { name: '保存', exact: true }).click()
  await saved
  await expect(edit).toBeHidden()
  expect(posts[0]).toEqual({
    rule_code: 'night-discount',
    rule_type: 'time_based',
    multiplier: '0.5',
    priority: 5,
    stacking_mode: 'exclusive',
    start_minute: 0,
    end_minute: 420,
    weekdays: [1, 5, 6],
    scope: { groups: ['vip'] },
  })
  expect('min_monthly_tokens' in posts[0] || 'min_monthly_spend_micro' in posts[0]).toBe(false)

  // 新建阶梯量规则：阈值 USD → micro；时段字段不发；空范围三键都不发
  await page.getByRole('button', { name: '新建规则' }).click()
  const create = page.getByRole('dialog')
  await create.locator('#r-type').selectOption('volume')
  await create.locator('#r-code').fill('heavy-users')
  await create.locator('#r-mult').fill('0.85')
  await create.locator('#r-thr').fill('1000000')
  await create.locator('#r-spend').fill('49.99')
  await create.locator('#r-stacking').selectOption('best_for_user')
  await expect(create.locator('#r-start')).toHaveCount(0)
  saved = page.waitForRequest((r) => r.method() === 'POST' && r.url().endsWith('/admin/pricing/rules'))
  await create.getByRole('button', { name: '保存', exact: true }).click()
  await saved
  expect(posts[1]).toEqual({
    rule_code: 'heavy-users',
    rule_type: 'volume',
    multiplier: '0.85',
    priority: 0,
    stacking_mode: 'best_for_user',
    min_monthly_tokens: 1_000_000,
    min_monthly_spend_micro: 49_990_000,
    scope: {},
  })

  // 列表：上下线打 toggle 端点并提示需发布；删除经确认框
  const toggled = page.waitForRequest((r) => r.method() === 'POST' && r.url().endsWith('/toggle'))
  await page.getByRole('row').filter({ hasText: 'night-discount' }).getByRole('button', { name: '停用', exact: true }).click()
  await toggled
  // 前面两次保存也各弹过一次"需发布"，取最新的一条即可
  await expect(page.getByRole('status').filter({ hasText: '需发布定价 epoch' }).last()).toBeVisible()
  expect(toggles).toEqual([{ path: '/admin/pricing/rules/night-discount/toggle', body: { enabled: false } }])
  await page.getByRole('row').filter({ hasText: 'night-discount' }).getByRole('button', { name: '删除', exact: true }).click()
  const confirm = page.getByRole('alertdialog')
  await expect(confirm).toContainText('删除 night-discount？')
  const removed = page.waitForRequest((r) => r.method() === 'DELETE' && r.url().includes('/admin/pricing/rules/'))
  await confirm.getByRole('button', { name: '删除', exact: true }).click()
  await removed
  expect(deletes).toEqual(['/admin/pricing/rules/night-discount'])
})

test('SMTP 卡：单键回显、去空格与 reply_to 空转 null、端口越界归零、未保存前测试按钮禁用、测试信按已保存配置发', async ({ page }) => {
  await prepare(page)
  const writes: Json[] = []
  const tests: Json[] = []
  let saved: Json | null = null
  await page.route('**/admin/settings/smtp', (route) => route.fulfill({ json: { value: saved } }))
  await page.route('**/admin/settings/smtp/test', async (route) => {
    tests.push(route.request().postDataJSON() as Json)
    await route.fulfill({ json: { ok: true } })
  })
  await page.route('**/admin/settings', async (route) => {
    if (route.request().isNavigationRequest() || route.request().method() !== 'POST') return route.fallback()
    const body = route.request().postDataJSON() as { key: string; value: Json }
    writes.push(body)
    if (body.key === 'smtp') saved = body.value
    await route.fulfill({ json: { ok: true } })
  })

  await page.goto('/admin/settings')
  await page.getByRole('tab', { name: '邮件（SMTP）' }).click()
  const panel = page.getByRole('tabpanel')
  const save = panel.getByRole('button', { name: '保存', exact: true })
  const send = panel.getByRole('button', { name: '发送测试邮件' })
  await expect(save).toBeDisabled()
  await expect(panel.getByText('先保存主机与发件地址')).toBeVisible()
  await expect(send).toBeDisabled()

  await panel.locator('#smtp-host').fill('  smtp.example.com ')
  await panel.locator('#smtp-port').fill('70000')
  await expect(panel.locator('#smtp-port')).toHaveValue('', { timeout: 2_000 })
  await panel.locator('#smtp-port').fill('465')
  await panel.getByRole('button', { name: '隐式 TLS', exact: true }).click()
  await panel.locator('#smtp-user').fill(' mailer ')
  await panel.locator('#smtp-pass').fill('s3cret')
  await panel.locator('#smtp-from').fill(' no-reply@example.com ')
  await panel.locator('#smtp-from-name').fill(' Okapi ')
  await panel.locator('#smtp-reply-to').fill('   ')
  const wrote = page.waitForRequest((r) => r.method() === 'POST' && r.url().endsWith('/admin/settings'))
  await save.click()
  await wrote
  await expect(page.getByRole('status').filter({ hasText: 'SMTP 设置已保存' })).toBeVisible()
  expect(writes).toEqual([{
    key: 'smtp',
    value: {
      host: 'smtp.example.com', port: 465, security: 'tls', username: 'mailer', password: 's3cret',
      from_address: 'no-reply@example.com', from_name: 'Okapi', reply_to: null,
    },
  }])

  // 保存后草稿清空、回显来自单键接口；测试信只能按已保存配置发，且收件人要像邮箱
  await expect(save).toBeDisabled()
  await expect(panel.locator('#smtp-host')).toHaveValue('smtp.example.com')
  await expect(panel.getByText('用已保存的配置发一封测试信')).toBeVisible()
  await panel.locator('#smtp-test-to').fill('not-mail')
  await expect(send).toBeDisabled()
  await panel.locator('#smtp-test-to').fill('me@example.com')
  const tested = page.waitForRequest((r) => r.method() === 'POST' && r.url().endsWith('/smtp/test'))
  await send.click()
  await tested
  await expect(page.getByRole('status').filter({ hasText: '测试邮件已发送至 me@example.com' })).toBeVisible()
  expect(tests).toEqual([{ to: 'me@example.com', lang: 'zh-CN' }])
  // 有未保存草稿时不许发测试信：测的是已保存配置，发了也是误导
  await panel.locator('#smtp-from-name').fill('Okapi Ops')
  await expect(send).toBeDisabled()
})

test('TOTP 绑定：开始绑定拿 otpauth 与 pending，验证码不足 6 位不放行，错码显示错误并可重试，确认后进入已开启态；无会话时降级提示', async ({ page }) => {
  await prepare(page, { permissions: [] })
  const confirms: Json[] = []
  await page.route('**/auth/totp/enroll', (route) =>
    route.fulfill({ json: { otpauth_url: 'otpauth://totp/Okapi:alice?secret=JBSWY3DPEHPK3PXP&issuer=Okapi', pending: 'pending-blob' } }),
  )
  await page.route('**/auth/totp/confirm', async (route) => {
    const body = route.request().postDataJSON() as { pending: string; code: string }
    confirms.push(body)
    if (body.code === '000000') await route.fulfill(apiError(400, 'totp_invalid'))
    else await route.fulfill({ json: { enabled: true } })
  })

  await page.goto('/portal/security')
  await page.getByRole('button', { name: '开始绑定' }).click()
  await expect(page.getByText('otpauth://totp/Okapi:alice?secret=JBSWY3DPEHPK3PXP&issuer=Okapi')).toBeVisible()
  const code = page.locator('#code')
  const confirm = page.getByRole('button', { name: '确认开启' })
  await code.fill('12345')
  await expect(confirm).toBeDisabled()
  await code.fill('000000')
  await confirm.click()
  await expect(page.getByText('两步验证码错误')).toBeVisible()
  await code.fill('654321')
  await confirm.click()
  // 成功态：卡片切成"已开启"提示（toast 也叫这个名，故按提示正文断言）
  await expect(page.getByText('下次邮箱密码登录时将要求输入验证器上的 6 位数字。')).toBeVisible()
  await expect(code).toHaveCount(0)
  expect(confirms).toEqual([
    { pending: 'pending-blob', code: '000000' },
    { pending: 'pending-blob', code: '654321' },
  ])

  // API Key 单轨登录没有会话 cookie：后端 401 → 引导改用邮箱密码登录，而不是留一个哑按钮
  await page.route('**/auth/totp/enroll', (route) => route.fulfill(apiError(401, 'unauthorized')))
  await page.reload()
  await page.getByRole('button', { name: '开始绑定' }).click()
  await expect(page.getByText(/两步验证需邮箱密码登录/)).toBeVisible()
  await expect(page.getByRole('button', { name: '开始绑定' })).toHaveCount(0)
})

test('重置密码：缺 token 直接提示无效；长度与一致性校验挡在提交前；成功回登录页；失效 token 提示重新申请', async ({ page }) => {
  await prepare(page, { signedIn: false })
  const posts: Json[] = []
  await page.route('**/auth/password/reset', async (route) => {
    const body = route.request().postDataJSON() as { token: string; password: string }
    posts.push(body)
    if (body.token === 'tok-expired') await route.fulfill(apiError(400, 'bad_request', 'reset_token_invalid'))
    else await route.fulfill({ json: { ok: true } })
  })

  await page.goto('/reset-password')
  await expect(page.getByRole('status').filter({ hasText: '重置链接无效或已过期' })).toBeVisible()
  await expect(page.locator('#reset-password')).toHaveCount(0)

  await page.goto('/reset-password?token=tok-fresh')
  const submit = page.getByRole('button', { name: '确认修改' })
  await page.locator('#reset-password').fill('short1')
  await page.locator('#reset-confirm').fill('short1')
  await expect(submit).toBeDisabled()
  await page.locator('#reset-password').fill('long-enough-1')
  await page.locator('#reset-confirm').fill('long-enough-2')
  await expect(page.getByText('两次输入的密码不一致')).toBeVisible()
  await expect(submit).toBeDisabled()
  await page.locator('#reset-confirm').fill('long-enough-1')
  await expect(submit).toBeEnabled()
  await submit.click()
  await expect(page.getByRole('status').filter({ hasText: '密码已更新' })).toBeVisible()
  expect(posts).toEqual([{ token: 'tok-fresh', password: 'long-enough-1' }])
  await page.getByRole('button', { name: '返回登录' }).click()
  await expect(page).toHaveURL(/\/$/)

  await page.goto('/reset-password?token=tok-expired')
  await page.locator('#reset-password').fill('long-enough-1')
  await page.locator('#reset-confirm').fill('long-enough-1')
  await page.getByRole('button', { name: '确认修改' }).click()
  await expect(page.getByRole('alert')).toContainText('重置链接无效或已过期')
  expect(posts).toHaveLength(2)
})
