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

test('API key 登录直达门户总览与三区布局', async ({ page, request }) => {
  await signInWithKey(page, (await sharedUser(request)).apiKey)
  // 三区布局：顶部栏常驻身份区（余额徽章）+ 内容区总览卡片，两者都应在
  await expect(page.getByText(/^(余额|Balance) \$/)).toBeVisible()
  await expect(page.getByRole('heading', { name: /^余额$|^Balance$/ })).toBeVisible()
  // 侧栏分组标题：宽屏下可见（窄屏折叠为抽屉）。
  // 用 i 标志：标题带 uppercase 样式，Playwright 匹配的是渲染后文本
  await expect(page.getByText(/额度与账单|billing/i).first()).toBeVisible()
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

