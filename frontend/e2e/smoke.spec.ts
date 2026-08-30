import { expect, test } from '@playwright/test'

// e2e 冒烟三链路：登录页渲染 / 公开价格页 / 注册→登录→兑 key→进门户（全真实 API）。

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

test('API key 登录直达门户总览', async ({ page, request }) => {
  // 全走真实 API：注册 → 登录会话 → 兑 key
  const suffix = Math.random().toString(36).slice(2, 10)
  const email = `e2e-${suffix}@ok.test`
  const reg = await request.post('/auth/register', {
    data: { email, username: `e2e-${suffix}`, password: 'hunter2-strong' },
  })
  expect(reg.ok()).toBeTruthy()
  const login = await request.post('/auth/login', {
    data: { email, password: 'hunter2-strong' },
  })
  expect(login.ok()).toBeTruthy()
  const keyResp = await request.post('/auth/keys', { data: { name: 'e2e' } })
  expect(keyResp.ok()).toBeTruthy()
  const { api_key: apiKey } = (await keyResp.json()) as { api_key: string }

  await page.goto('/')
  await page.getByRole('button', { name: 'API Key' }).click()
  await page.locator('#key').fill(apiKey)
  await page.getByRole('button', { name: /^登录$|^Sign in$/ }).click()
  await expect(page).toHaveURL(/\/portal/)
  await expect(page.getByText(/余额|Balance/)).toBeVisible()
})
