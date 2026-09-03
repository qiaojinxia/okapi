import { expect, test } from '@playwright/test'
import type { APIRequestContext } from '@playwright/test'

// e2e 冒烟：登录页 / 公开价格页 / 权限分级 / session 降级 / 登出清会话 / 门户总览
// （全打真实 API，不 mock）。
//
// 注册次数刻意收敛：/auth/register 有每 IP 限流（对齐 new-api rc.24），若每个用例
// 各注册一个用户，反复调试就会撞上限而红——那种红会掩盖真实失败。故三个只读用例
// 共用一个普通用户，只有"登出"用例独立（它会主动失效 session，不能共享）。
const PASSWORD = 'hunter2-strong'

interface TestUser {
  email: string
  apiKey: string
}
let shared: TestUser | null = null

/// 全文件共用一个测试账号：注册一次，其后各用例按需取 key 或用邮箱密码重新登录。
///
/// 登出用例也复用这套凭据——同一账号可多次登录建新 session，而其余用例走 API key，
/// 不受登出影响（登出只清 session）。如此一轮仅注册 1 次，可反复跑不撞限流。
async function sharedUser(request: APIRequestContext): Promise<TestUser> {
  if (shared !== null) return shared
  const suffix = Math.random().toString(36).slice(2, 10)
  const email = `e2e-${suffix}@ok.test`
  // 关键接口限流按 IP 计数（对齐 new-api rc.24），register 默认 5/分钟。
  // 每轮换一个 x-real-ip，使反复跑 e2e 不会累计到上限——否则开发期的红灯
  // 全是限流噪声，真实回归反倒被淹没。
  const headers = { 'x-real-ip': `203.0.113.${1 + Math.floor(Math.random() * 250)}` }
  const reg = await request.post('/auth/register', {
    headers,
    data: { email, username: `e2e-${suffix}`, password: PASSWORD },
  })
  expect(reg.ok(), `注册失败（${reg.status()}）`).toBeTruthy()
  const login = await request.post('/auth/login', {
    headers,
    data: { email, password: PASSWORD },
  })
  expect(login.ok()).toBeTruthy()
  const keyResp = await request.post('/auth/keys', { data: { name: 'e2e' } })
  expect(keyResp.ok()).toBeTruthy()
  const { api_key: apiKey } = (await keyResp.json()) as { api_key: string }
  shared = { email, apiKey }
  return shared
}

/// 用 API key 登录并落在门户（三处用例共用的前置动作）。
async function signInWithKey(page: import('@playwright/test').Page, apiKey: string) {
  await page.goto('/')
  await page.getByRole('button', { name: 'API Key' }).click()
  await page.locator('#key').fill(apiKey)
  await page.getByRole('button', { name: /^登录$|^Sign in$/ }).click()
  await expect(page).toHaveURL(/\/portal/)
}

test('登录页渲染两种登录方式与价格页链接', async ({ page }) => {
  await page.goto('/')
  await expect(page.getByRole('button', { name: /邮箱登录|Email login/ })).toBeVisible()
  await expect(page.getByRole('button', { name: 'API Key' })).toBeVisible()
  await expect(page.getByRole('link', { name: /模型价格|Model pricing/ })).toBeVisible()
})

test('公开价格页无鉴权可达且渲染表头', async ({ page }) => {
  await page.goto('/pricing')
  await expect(page.getByRole('heading', { name: /模型价格|Model pricing/ })).toBeVisible()
  await expect(page.getByRole('columnheader', { name: /^输入$|^Input$/ })).toBeVisible()
  // 缓存双轨：读取与写入分列展示（DESIGN §3.2）
  await expect(page.getByRole('columnheader', { name: /缓存读取|Cache read/ })).toBeVisible()
  await expect(page.getByRole('columnheader', { name: /缓存写入|Cache write/ })).toBeVisible()
  // 1K/1M 计价单位切换（对齐 new-api 展示习惯）
  await expect(page.getByLabel(/计价单位|Price unit/)).toBeVisible()
  await expect(page.getByText(/定价模拟器|Pricing simulator/)).toBeVisible()
})

test('权限分级：普通用户被管理面拒绝，且前端不因 403 崩溃', async ({ page, request }) => {
  const { apiKey } = await sharedUser(request)

  // 后端侧：新注册用户（role=1）对每类管理面一律 403
  for (const path of [
    '/admin/users',
    '/admin/models',
    '/admin/keys',
    '/admin/settings',
    '/admin/permissions',
    '/admin/stats/overview',
  ]) {
    const resp = await request.get(path, { headers: { authorization: `Bearer ${apiKey}` } })
    expect(resp.status(), `${path} 必须拒绝普通用户`).toBe(403)
  }

  // 前端侧：403 应呈现为错误文案而非白屏/异常
  await signInWithKey(page, apiKey)
  await page.goto('/admin/users')
  // 页面骨架仍渲染（标题可见），数据区给出可读的权限错误
  await expect(page.getByText(/权限|permission/i).first()).toBeVisible({ timeout: 10_000 })

  // 导航按权限裁剪：无权的入口不该出现，而不是点进去再吃 403。
  // 普通用户 permissions=[] → 所有带 permission 标注的管理入口都应消失。
  const sidebar = page.getByRole('complementary')
  for (const label of [/渠道池|Channel pools/, /价格分组|Price groups/, /角色与权限|Roles & permissions/]) {
    await expect(sidebar.getByRole('link', { name: label })).toHaveCount(0)
  }
  // 不需要权限的入口仍在（否则就是把导航整个裁没了）
  await expect(sidebar.getByRole('link', { name: /门户|Portal/ })).toBeVisible()
})

test('session 鉴权面在 key 单轨下降级提示而非空白', async ({ page, request }) => {
  // Team 与 TOTP 走 web session（成员自助），API Key 登录的浏览器没有 cookie 会 401；
  // 页面必须引导改用邮箱密码登录，而不是显示"没有团队"或哑按钮。
  await signInWithKey(page, (await sharedUser(request)).apiKey)

  // 浏览器上下文无 session cookie（key 单轨登录），两个页面都应给出降级提示
  await page.goto('/portal/teams')
  await expect(page.getByText(/邮箱密码登录|email\/password login/i)).toBeVisible({
    timeout: 10_000,
  })
  await page.goto('/portal/security')
  await expect(page.getByRole('button', { name: /开始绑定|Start binding/ })).toBeVisible()
  // 最近登录卡：该用户注册后走过邮箱登录（sharedUser），至少一条成功记录
  await expect(page.getByText(/最近登录|Recent sign-ins/)).toBeVisible()
  await expect(page.getByText(/^成功$|^OK$/).first()).toBeVisible({ timeout: 10_000 })
})

test('登出同时清服务端 session（共享设备不留残留会话）', async ({ page, request }) => {
  // 邮箱密码登录会建 Redis session；只清本地 key 会留下可用 session，
  // 下一个人仍能操作 Team / TOTP 等 session 鉴权页面。
  const { email } = await sharedUser(request)

  await page.goto('/')
  await page.getByRole('button', { name: /邮箱登录|Email login/ }).click()
  await page.locator('#email').fill(email)
  await page.locator('#password').fill(PASSWORD)
  await page.getByRole('button', { name: /^登录$|^Sign in$/ }).click()
  await expect(page).toHaveURL(/\/portal/)

  // 登录后 session 有效：session 鉴权端点可达
  const before = await page.request.get('/api/teams')
  expect(before.status(), 'session 应有效').toBe(200)

  await page.getByRole('button', { name: /退出登录|Sign out/ }).click()
  await expect(page).toHaveURL(/^http:\/\/[^/]+\/$/)

  const after = await page.request.get('/api/teams')
  expect(after.status(), '登出必须使服务端 session 失效').toBe(401)
})

test('API key 登录直达门户总览：六卡 KPI + 三页签零请求切换', async ({ page, request }) => {
  await signInWithKey(page, (await sharedUser(request)).apiKey)
  // 三区布局：顶部栏常驻身份区（余额徽章）+ 内容区 KPI 卡，两者都应在
  await expect(page.getByText(/^(余额|Balance) \$/)).toBeVisible()
  const main = page.getByRole('main')
  // 六张 KPI 的标签（new-api 数据看板对齐 + "已为你节省"是本站特有的让利呈现）
  for (const label of [/^余额$|^Balance$/, /周期消费|Period spend/, /已为你节省|Saved for you/, /^请求数$|^Requests$/]) {
    await expect(main.getByText(label).first()).toBeVisible()
  }
  // 侧栏分组标题：宽屏下可见（窄屏折叠为抽屉）。
  // 用 i 标志：标题带 uppercase 样式，Playwright 匹配的是渲染后文本
  await expect(page.getByText(/额度与账单|billing/i).first()).toBeVisible()

  // 三个页签是同一份数据的不同切法：新用户零调用，每签都应给出"还没有调用"的空态
  // 而非报错或白屏
  for (const tab of [/模型分布|By model/, /Token 构成|Token mix/, /消费趋势|Spend trend/]) {
    await page.getByRole('tab', { name: tab }).click()
    await expect(main.getByText(/还没有调用|No calls in this window/)).toBeVisible()
  }
  // 范围切换（本密钥 / 全账户）不报错
  await page.getByRole('button', { name: /全账户|Whole account/ }).click()
  await expect(main.getByText(/还没有调用|No calls in this window/)).toBeVisible()
})

test('门户日志页：过滤卡 + 空态；账户流水页：两页签空态；充值页挂流水入口', async ({ page, request }) => {
  await signInWithKey(page, (await sharedUser(request)).apiKey)

  // 日志页：范围开关（缺省本密钥）、只看失败、模型过滤都在；新用户空态给下一步提示
  await page.goto('/portal/logs')
  await expect(page.getByText(/^范围$|^Scope$/)).toBeVisible()
  await expect(page.getByRole('switch', { name: /只看失败|Errors only/ })).toBeVisible()
  await expect(page.getByText(/还没有调用|No calls in this window/)).toBeVisible()
  // 导出按钮在空表时禁用——没有行可导不该给一个会下载空文件的按钮
  await expect(page.getByRole('button', { name: /导出 CSV|Export CSV/ })).toBeDisabled()

  // 账户流水：余额变动 / 充值订单 两签，各自空态
  // （顶部栏也有同名 h1——那是"当前页标题"，限定 main 才是页内标题）
  await page.goto('/portal/ledger')
  await expect(
    page.getByRole('main').getByRole('heading', { name: /账户流水|Account ledger/ }),
  ).toBeVisible()
  await expect(page.getByText(/还没有余额变动|No balance changes yet/)).toBeVisible()
  await page.getByRole('tab', { name: /充值订单|Top-up orders/ }).click()
  await expect(page.getByText(/还没有充值订单|No top-up orders yet/)).toBeVisible()

  // 充值页：付完钱最常见的追问是"到账了吗"，答案所在页面的入口就在提问处
  await page.goto('/portal/topup')
  await page.getByRole('link', { name: /查看订单与到账记录|View orders & credits/ }).click()
  await expect(page).toHaveURL(/\/portal\/ledger/)
})

/// 演示超管（scripts/dev-reset.sh 灌注，凭据确定）。库里没有就整组跳过——
/// 在别人的环境里制造"假红"比少一组覆盖更糟。登录一次拿 session，再兑一把 admin key。
/// 返回 key 与其 id：用例结束要把这把 key 删掉，否则每跑一轮演示超管就多一把垃圾 key。
async function demoAdminKey(
  request: APIRequestContext,
): Promise<{ apiKey: string; keyId: number } | null> {
  const login = await request.post('/auth/login', {
    headers: { 'x-real-ip': `198.51.100.${1 + Math.floor(Math.random() * 250)}` },
    data: { email: 'root@okapi.local', password: 'okapi-demo-2026' },
  })
  if (!login.ok()) return null
  const keyResp = await request.post('/auth/keys', { data: { name: `e2e-admin-${Date.now()}` } })
  if (!keyResp.ok()) return null
  const body = (await keyResp.json()) as { api_key: string; key_id: number }
  return { apiKey: body.api_key, keyId: body.key_id }
}

test('管理端：总览实时条 + 健康芯片 + 日志页统计条/过滤 + 洞察三页', async ({
  page,
  request,
}) => {
  const admin = await demoAdminKey(request)
  test.skip(admin === null, '当前库无演示超管（scripts/dev-reset.sh 未跑），跳过管理端冒烟')
  const { apiKey: adminKey, keyId } = admin as { apiKey: string; keyId: number }

  await page.goto('/')
  await page.getByRole('button', { name: 'API Key' }).click()
  await page.locator('#key').fill(adminKey)
  await page.getByRole('button', { name: /^登录$|^Sign in$/ }).click()
  await expect(page).toHaveURL(/\/portal/)

  // 总览：实时条（Redis 秒桶，CH 无关）、五张 KPI、"需要注意"卡头四枚组件芯片
  await page.goto('/admin')
  const main = page.getByRole('main')
  await expect(main.getByText(/^实时$|^Live$/)).toBeVisible()
  await expect(main.getByText('QPS')).toBeVisible()
  await expect(main.getByText(/^请求数$|^Requests$/).first()).toBeVisible()
  for (const chip of ['PG', 'Redis', 'CH', 'NATS']) {
    await expect(main.getByText(chip, { exact: true })).toBeVisible()
  }

  // 日志页：统计条 RPM/TPM 标签 + 过滤器 + 窗口选择器 + 导出按钮
  await page.goto('/admin/logs')
  await expect(main.getByText('RPM', { exact: true })).toBeVisible({ timeout: 10_000 })
  await expect(main.getByText('TPM', { exact: true })).toBeVisible()
  await expect(main.getByRole('switch', { name: /只看失败|Errors only/ })).toBeVisible()
  await expect(main.getByRole('button', { name: /^7 天$|^7 days$/ })).toBeVisible()
  // 深链：带 errors_only 的地址落地后开关应为开——URL 是过滤条件的唯一真相
  await page.goto('/admin/logs?errors_only=true&hours=168')
  await expect(main.getByRole('switch', { name: /只看失败|Errors only/ })).toBeChecked()

  // 用量分析：三视图可切且进 URL；过滤深链落地即出芯片、KPI 环比条常驻
  await page.goto('/admin/stats')
  await expect(main.getByText(/对比上一个|vs previous/).first()).toBeVisible({ timeout: 10_000 })
  for (const [tab, view] of [
    [/^拆分$|^Breakdown$/, 'breakdown'],
    [/^流向$|^Flow$/, 'flow'],
    [/^趋势$|^Trend$/, undefined],
  ] as const) {
    await main.getByRole('tab', { name: tab }).click()
    await expect(main.getByRole('tab', { name: tab })).toHaveAttribute('aria-selected', 'true')
    if (view !== undefined) await expect(page).toHaveURL(new RegExp(`view=${view}`))
  }
  // 拆分签：按渠道拆分时"渠道"分段生效且表头随维度变化
  await page.goto('/admin/stats?view=breakdown&by=channel&days=30')
  await expect(main.getByRole('button', { name: /^渠道$|^Channel$/, pressed: true })).toBeVisible({
    timeout: 10_000,
  })
  // 过滤深链：model 过滤芯片出现，且"模型"维度被置灰（再按它拆只剩一行）
  await page.goto('/admin/stats?view=breakdown&model=nonexistent-model-e2e')
  await expect(main.getByText('nonexistent-model-e2e')).toBeVisible()
  await expect(main.getByRole('button', { name: /^模型$|^Model$/, pressed: false })).toBeDisabled()

  // 服务质量与经营报表：从旧统计页拆出的两页，各自页签可切
  await page.goto('/admin/quality')
  for (const tab of [/渠道健康|Channel health/, /模型时延|Model latency/, /错误分布|Error breakdown/, /客户端分布|Client breakdown/]) {
    await main.getByRole('tab', { name: tab }).click()
    await expect(main.getByRole('tab', { name: tab })).toHaveAttribute('aria-selected', 'true')
  }
  await page.goto('/admin/revenue')
  await main.getByRole('tab', { name: /用户消耗排行|Top spenders/ }).click()
  await main.getByRole('tab', { name: /收入与让利|Revenue/ }).click()
  // 收入签里"按分组"表与"资金流入"行至少有一个渲染（演示库有 default/vip/free 三组）
  await expect(main.getByText(/资金流入|Cash inflow/)).toBeVisible({ timeout: 10_000 })

  // 总览站点规模条（PG-only）：四项存量都在
  await page.goto('/admin')
  await expect(main.getByText(/站点规模|Site inventory/)).toBeVisible({ timeout: 10_000 })
  await expect(main.getByText(/^活跃密钥$|^Active keys$/)).toBeVisible()

  // 审计页：本用例开头的邮箱登录已落 user.login，按动作过滤深链落地即见；
  // 点行展开详情（IP / UA 键值行）
  await page.goto('/admin/audit?action=user.login&target=root@okapi.local')
  await expect(main.getByRole('columnheader', { name: /^动作$|^Action$/ })).toBeVisible({
    timeout: 10_000,
  })
  const auditRow = main.getByRole('row').filter({ hasText: 'user.login' }).first()
  await expect(auditRow).toBeVisible()
  await auditRow.click()
  await expect(main.getByText(/^ip$/).first()).toBeVisible()

  // 渠道页：渠道级"启用"之外要看得见 key 状态机汇总与近 24h 健康两列
  await page.goto('/admin/channels')
  await expect(main.getByRole('columnheader', { name: /最近测试|Last test/ })).toBeVisible()
  await expect(main.getByRole('columnheader', { name: /近 24h|Last 24h/ })).toBeVisible()

  // 运维页死信签：待办里"到运维页重投或排查"必须真有落地——列表 + 待处理计数徽章
  await page.goto('/admin/ops')
  await main.getByRole('tab', { name: /死信队列|Dead-letter queue/ }).click()
  await expect(main.getByText(/条待处理|pending/).first()).toBeVisible({ timeout: 10_000 })
  await expect(main.getByRole('button', { name: /^重投|^Requeue/ })).toBeDisabled()

  // 用户抽屉：落地签是"用量"（先看行为再动手），且含"最近余额变动"块。
  // 开发库有上千个测试用户，rootadmin 不在首页——走页面自己的搜索（回车检索）
  await page.goto('/admin/users')
  await page.locator('#u-search').fill('rootadmin')
  await page.locator('#u-search').press('Enter')
  const row = main.getByRole('row').filter({ hasText: 'rootadmin' })
  await expect(row).toBeVisible({ timeout: 10_000 })
  await row.getByRole('button', { name: /^管理$|^Manage$/ }).click()
  const drawer = page.getByRole('dialog')
  await expect(drawer.getByRole('tab', { name: /^用量$|^Usage$/ })).toHaveAttribute(
    'aria-selected',
    'true',
  )
  await expect(drawer.getByText(/最近余额变动|Recent balance changes/)).toBeVisible({
    timeout: 10_000,
  })

  // 清理：删掉本轮兑的 admin key（session 鉴权，前面登录的 request 上下文仍有效）
  const cleanup = await request.delete(`/api/me/keys/${keyId}`, {
    headers: { authorization: `Bearer ${adminKey}` },
  })
  expect(cleanup.ok(), `清理 e2e key 失败（${cleanup.status()}）`).toBeTruthy()
})

test('站点公告：发布后登录页即见、可关闭并记住、下架后消失', async ({ page, request }) => {
  const admin = await demoAdminKey(request)
  test.skip(admin === null, '当前库无演示超管，跳过公告冒烟')
  const { apiKey, keyId } = admin as { apiKey: string; keyId: number }
  const auth = { authorization: `Bearer ${apiKey}` }
  const stamp = `e2e-${Date.now()}`

  // 发布（后端公开端点有 60s 进程缓存；e2e 的 console 是刚起的进程，首读即新值）
  const publish = await request.post('/admin/settings', {
    headers: auth,
    data: {
      key: 'site_notice',
      value: { enabled: true, title: stamp, body: '维护窗口 02:00-03:00', level: 'warning', updated_at: stamp },
    },
  })
  expect(publish.ok(), `发布失败（${publish.status()}）`).toBeTruthy()

  try {
    // 未登录的登录页就该看到——停服通知对还没登录的人同样成立
    await page.goto('/')
    const banner = page.getByRole('status').filter({ hasText: stamp })
    await expect(banner).toBeVisible({ timeout: 10_000 })
    // 关闭后同一版不再出现，刷新也不回来
    await banner.getByRole('button', { name: /^关闭$|^Close$/ }).click()
    await expect(banner).toBeHidden()
    await page.reload()
    await expect(page.getByRole('status').filter({ hasText: stamp })).toHaveCount(0)
  } finally {
    // 下架 + 清理 key：无论断言成败都不能把 e2e 公告留在共享环境里
    await request.post('/admin/settings', {
      headers: auth,
      data: { key: 'site_notice', value: { enabled: false, title: '', body: '', level: 'info' } },
    })
    await request.delete(`/api/me/keys/${keyId}`, { headers: auth })
  }
})

test('删除密钥需二次确认：直接点删除不生效，取消后密钥仍在', async ({ page, request }) => {
  // 删除不可逆（密钥一删，用它的调用立刻全挂），此前点即执行——列表里相邻两行的
  // 删除按钮只差几十像素，误点没有挽回机会。用门户自助删密钥这条普通用户权限内的
  // 路径验证确认框；管理面（渠道/模型/角色）用的是同一组件同一行为。
  const user = await sharedUser(request)

  // 先重新登录：前面的登出用例会让共享 request 上的 session 失效，
  // 而建密钥走 session 鉴权。登录不限流（只有 register 限），可安全重来。
  const relogin = await request.post('/auth/login', {
    data: { email: user.email, password: PASSWORD },
  })
  expect(relogin.ok(), `重新登录失败（${relogin.status()}）`).toBeTruthy()

  // 造一个专供删除的密钥，避免动到其他用例赖以登录的那把
  const victim = `e2e-confirm-${Date.now()}`
  const created = await request.post('/auth/keys', { data: { name: victim } })
  expect(created.ok(), `建密钥失败（${created.status()}）`).toBeTruthy()

  await signInWithKey(page, user.apiKey)
  await page.goto('/portal/keys')
  const row = page.getByRole('row').filter({ hasText: victim })
  await expect(row).toBeVisible()

  // 点删除：只应弹确认框，密钥不能就这么消失
  await row.getByRole('button', { name: /^删除$|^Delete$/ }).click()
  const dialog = page.getByRole('alertdialog')
  await expect(dialog).toBeVisible()
  await expect(row).toBeVisible()

  // 高危项要求手输名称，没输之前确认按钮必须禁用
  await expect(dialog.getByRole('button', { name: /^删除$|^Delete$/ })).toBeDisabled()

  // 取消后密钥仍在（确认框自身无副作用）
  await dialog.getByRole('button', { name: /^取消$|^Cancel$/ }).click()
  await expect(dialog).toBeHidden()
  await page.reload()
  await expect(page.getByRole('row').filter({ hasText: victim })).toBeVisible()

  // 输对名称后才放行，删除真正生效
  await page.getByRole('row').filter({ hasText: victim }).getByRole('button', { name: /^删除$|^Delete$/ }).click()
  const dialog2 = page.getByRole('alertdialog')
  await dialog2.locator('#confirm-text').fill(victim)
  await dialog2.getByRole('button', { name: /^删除$|^Delete$/ }).click()
  await expect(page.getByRole('row').filter({ hasText: victim })).toBeHidden()
})

