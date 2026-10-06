import { expect, test } from '@playwright/test'
import type { Page } from '@playwright/test'
import { fileURLToPath } from 'node:url'

// 第 3 节第 13 条此前没有写操作 e2e 的列表级 / 运维 / 设置 / 兑换 / 门户密钥动作。
// 接口桩，不碰数据库；断言请求体形状。

type Json = Record<string, unknown>

async function prepare(page: Page, { permissions = ['*'], signedIn = true } = {}) {
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
}

async function openedDialog(page: Page) {
  const dialog = page.getByRole('dialog')
  await expect(dialog.locator(':focus')).toHaveCount(1)
  return dialog
}

function apiError(status: number, code: string, param?: string) {
  return { status, json: { error: { code, ...(param === undefined ? {} : { param }) } } }
}

async function dismissToasts(page: Page) {
  const close = page.getByRole('status').getByRole('button', { name: '关闭', exact: true })
  while ((await close.count()) > 0) {
    await close.first().click()
  }
  const alertClose = page.getByRole('alert').getByRole('button', { name: '关闭', exact: true })
  while ((await alertClose.count()) > 0) {
    await alertClose.first().click()
  }
}

async function prepareNotice(page: Page, value: unknown = null, language = 'zh-CN') {
  await prepare(page)
  await page.addInitScript((lang) => localStorage.setItem('okapi.lang', lang), language)
  const state = { value, reads: 0, posts: [] as Json[], readStatus: 200, saveStatus: 200, readGate: Promise.resolve(), saveGate: Promise.resolve() }
  await page.route('**/admin/settings/site_notice', async (route) => {
    state.reads += 1
    const snapshot = state.value
    await state.readGate
    return route.fulfill(state.readStatus === 200 ? { json: { value: snapshot } } : apiError(state.readStatus, 'internal_error'))
  })
  await page.route('**/admin/settings', async (route) => {
    if (route.request().isNavigationRequest() || route.request().method() !== 'POST') return route.fallback()
    const body = route.request().postDataJSON() as Json
    expect(body.key).toBe('site_notice')
    state.posts.push(body)
    await state.saveGate
    if (state.saveStatus !== 200) return route.fulfill(apiError(state.saveStatus, 'internal_error'))
    state.value = body.value
    return route.fulfill({ json: { ok: true } })
  })
  return state
}

async function pickChannels(page: Page, names: string[]) {
  await expect(page.getByRole('toolbar', { name: /已选/ })).toHaveCount(0)
  for (const name of names) {
    await page.getByRole('checkbox', { name: `勾选 ${name}` }).check()
  }
  await expect(page.getByRole('toolbar', { name: `已选 ${names.length} 项` })).toBeVisible()
}

const CHANNEL_ON = {
  id: 42,
  name: 'openai-main',
  provider: 'openai',
  api_base: 'https://api.openai.com/v1',
  status: 1,
  priority: 0,
  models: ['gpt-5'],
  keys: [{ status: 1 }],
  settings: {},
  pools: ['default'],
  pool_members: [{ pool_code: 'default', priority_override: null, weight_override: null }],
  cost_milli: 1000,
  data_retention: null,
  last_test: null,
  last_balance: null,
}

const CHANNEL_OFF = {
  ...CHANNEL_ON,
  id: 43,
  name: 'spare-compat',
  provider: 'openai_compat',
  status: 2,
  models: ['gpt-4o-mini'],
}

test('渠道列表：复制名为 -copy、行测活带第一个模型、测全部只打启用且空体、批量启停删、单删手输名称', async ({
  page,
}) => {
  await prepare(page)
  const calls: { path: string; method: string; body: Json | null }[] = []
  const record = (route: import('@playwright/test').Route) => {
    const r = route.request()
    calls.push({
      path: new URL(r.url()).pathname,
      method: r.method(),
      body: (r.postDataJSON() ?? null) as Json | null,
    })
  }
  await page.route('**/admin/channels?*', (route) => {
    const url = new URL(route.request().url())
    const enabled = url.searchParams.get('status') === '1'
    const data = enabled ? [CHANNEL_ON] : [CHANNEL_ON, CHANNEL_OFF]
    return route.fulfill({ json: { data, total: data.length, enabled: 1 } })
  })
  await page.route('**/admin/channels/batch', async (route) => {
    record(route)
    await route.fulfill({ json: { affected: 2 } })
  })
  await page.route('**/admin/channels/*/duplicate', async (route) => {
    record(route)
    await route.fulfill({ json: { ok: true } })
  })
  await page.route('**/admin/channels/*/test', async (route) => {
    record(route)
    const body = (route.request().postDataJSON() ?? {}) as { model?: string }
    if (body.model) await route.fulfill({ json: { ok: true, latency_ms: 12 } })
    else await route.fulfill({ json: { ok: false, error_code: 'timeout' } })
  })
  await page.route('**/admin/channels/*/status', async (route) => {
    record(route)
    await route.fulfill({ json: { ok: true } })
  })
  await page.route(/\/admin\/channels\/\d+$/, async (route) => {
    if (route.request().method() !== 'DELETE') return route.fallback()
    record(route)
    await route.fulfill({ json: { ok: true } })
  })

  await page.goto('/admin/channels')
  await expect(page.locator('#main-content').getByRole('heading', { name: '渠道' })).toBeVisible()
  const openai = page.getByRole('row').filter({ hasText: 'openai-main' })
  const spare = page.getByRole('row').filter({ hasText: 'spare-compat' })

  await openai.getByRole('button', { name: '复制', exact: true }).click()
  await expect.poll(() => calls.length).toBe(1)
  expect(calls[0]).toEqual({
    path: '/admin/channels/42/duplicate',
    method: 'POST',
    body: { name: 'openai-main-copy' },
  })
  await dismissToasts(page)
  await page.route('**/admin/channels/*/duplicate', (route) => route.fulfill(apiError(500, 'internal_error')))
  await openai.getByRole('button', { name: '复制', exact: true }).click()
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误' })).toBeVisible()
  await page.getByRole('alert').getByRole('button', { name: '关闭', exact: true }).click()
  await page.route('**/admin/channels/*/duplicate', async (route) => {
    record(route)
    await route.fulfill({ json: { ok: true } })
  })

  await openai.getByRole('button', { name: '测活', exact: true }).click()
  await expect.poll(() => calls.length).toBe(2)
  expect(calls[1]).toEqual({
    path: '/admin/channels/42/test',
    method: 'POST',
    body: { model: 'gpt-5' },
  })
  await expect(page.getByRole('status').filter({ hasText: /连通正常（12 ms）/ })).toBeVisible()

  await openai.getByRole('button', { name: '停用', exact: true }).click()
  await expect.poll(() => calls.length).toBe(3)
  expect(calls[2]).toEqual({
    path: '/admin/channels/42/status',
    method: 'POST',
    body: { status: 2 },
  })
  await dismissToasts(page)
  await page.route('**/admin/channels/*/status', (route) => route.fulfill(apiError(500, 'internal_error')))
  await openai.getByRole('button', { name: '停用', exact: true }).click()
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误' })).toBeVisible()
  await dismissToasts(page)
  await page.route('**/admin/channels/*/status', async (route) => {
    record(route)
    await route.fulfill({ json: { ok: true } })
  })

  await page.getByRole('button', { name: '测试全部启用渠道', exact: true }).click()
  await expect.poll(() => calls.some((c) => c.path.endsWith('/test') && Object.keys(c.body ?? {}).length === 0)).toBe(
    true,
  )
  const testAll = calls.find((c) => c.path === '/admin/channels/42/test' && JSON.stringify(c.body) === '{}')
  expect(testAll).toEqual({ path: '/admin/channels/42/test', method: 'POST', body: {} })
  await expect(page.getByRole('status').filter({ hasText: /测试完成：0 可达 \/ 1 失败（共 1）/ })).toBeVisible()
  await dismissToasts(page)

  await page.route('**/admin/channels/*/test', async (route) => {
    record(route)
    await route.fulfill({
      json: {
        ok: false,
        scope: 'model',
        error_code: 'model_not_found',
        http_status: 404,
        upstream_body: 'unknown model xyz',
      },
    })
  })
  await openai.getByRole('button', { name: '测活', exact: true }).click()
  await expect(page.getByRole('alert').filter({ hasText: /模型 gpt-5 调不通（model_not_found）：unknown model xyz/ })).toBeVisible()
  await dismissToasts(page)

  await page.route('**/admin/channels/*/test', async (route) => {
    record(route)
    await route.fulfill({ json: { ok: false, error_code: 'upstream_timeout' } })
  })
  await openai.getByRole('button', { name: '测活', exact: true }).click()
  await expect(page.getByRole('alert').filter({ hasText: /测活失败：upstream_timeout/ })).toBeVisible()
  await dismissToasts(page)

  await page.route('**/admin/channels/*/test', (route) => route.fulfill(apiError(500, 'internal_error')))
  await openai.getByRole('button', { name: '测活', exact: true }).click()
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误' })).toBeVisible()
  await dismissToasts(page)
  await page.route('**/admin/channels/*/test', async (route) => {
    record(route)
    await route.fulfill({ json: { ok: false, error_code: 'upstream_timeout' } })
  })

  await page.getByRole('checkbox', { name: '全选本页' }).check()
  await expect(page.getByRole('toolbar', { name: '已选 2 项' })).toBeVisible()
  await page.getByRole('toolbar', { name: '已选 2 项' }).getByRole('button', { name: '停用', exact: true }).click()
  await expect.poll(() => calls.filter((c) => c.path === '/admin/channels/batch').length).toBe(1)
  expect(calls.find((c) => c.path === '/admin/channels/batch')).toEqual({
    path: '/admin/channels/batch',
    method: 'POST',
    body: { ids: [42, 43], action: 'disable' },
  })
  await expect(page.getByRole('status').filter({ hasText: '已处理 2 个渠道' })).toBeVisible()
  await dismissToasts(page)

  await pickChannels(page, ['openai-main', 'spare-compat'])
  await page.route('**/admin/channels/batch', (route) => route.fulfill(apiError(500, 'internal_error')))
  await page.getByRole('toolbar', { name: '已选 2 项' }).getByRole('button', { name: '启用', exact: true }).click()
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误' })).toBeVisible()
  await dismissToasts(page)
  await page.route('**/admin/channels/batch', async (route) => {
    record(route)
    await route.fulfill({ json: { affected: 2 } })
  })
  await page.getByRole('toolbar', { name: '已选 2 项' }).getByRole('button', { name: '启用', exact: true }).click()
  await expect.poll(() => calls.filter((c) => c.path === '/admin/channels/batch').length).toBe(2)
  expect(calls.filter((c) => c.path === '/admin/channels/batch').at(-1)?.body).toEqual({
    ids: [42, 43],
    action: 'enable',
  })
  await dismissToasts(page)

  await pickChannels(page, ['openai-main', 'spare-compat'])
  await page.getByRole('toolbar', { name: '已选 2 项' }).getByRole('button', { name: '删除', exact: true }).click()
  const batchConfirm = page.getByRole('alertdialog')
  await expect(batchConfirm).toContainText('删除 2？')
  await page.route('**/admin/channels/batch', (route) => route.fulfill(apiError(500, 'internal_error')))
  await batchConfirm.getByRole('button', { name: '删除', exact: true }).click()
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误' })).toBeVisible()
  await dismissToasts(page)
  await page.route('**/admin/channels/batch', async (route) => {
    record(route)
    await route.fulfill({ json: { affected: 2 } })
  })
  await page.getByRole('toolbar', { name: '已选 2 项' }).getByRole('button', { name: '删除', exact: true }).click()
  await page.getByRole('alertdialog').getByRole('button', { name: '删除', exact: true }).click()
  await expect.poll(() => calls.filter((c) => c.path === '/admin/channels/batch').length).toBe(3)
  expect(calls.filter((c) => c.path === '/admin/channels/batch').at(-1)?.body).toEqual({
    ids: [42, 43],
    action: 'delete',
  })

  await spare.getByRole('button', { name: '删除', exact: true }).click()
  const one = page.getByRole('alertdialog')
  await expect(one.getByRole('button', { name: '删除', exact: true })).toBeDisabled()
  await one.locator('#confirm-text').fill('spare-compat')
  await page.route(/\/admin\/channels\/\d+$/, (route) => {
    if (route.request().method() !== 'DELETE') return route.fallback()
    return route.fulfill(apiError(500, 'internal_error'))
  })
  await one.getByRole('button', { name: '删除', exact: true }).click()
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误' })).toBeVisible()
  await dismissToasts(page)
  await page.route(/\/admin\/channels\/\d+$/, async (route) => {
    if (route.request().method() !== 'DELETE') return route.fallback()
    record(route)
    await route.fulfill({ json: { ok: true } })
  })
  await spare.getByRole('button', { name: '删除', exact: true }).click()
  const retry = page.getByRole('alertdialog')
  await retry.locator('#confirm-text').fill('spare-compat')
  await retry.getByRole('button', { name: '删除', exact: true }).click()
  await expect.poll(() => calls.some((c) => c.method === 'DELETE' && c.path === '/admin/channels/43')).toBe(true)
})

test('运维死信：只勾待处理；重投与丢弃都经确认框，体是 ids 数组', async ({ page }) => {
  await prepare(page)
  const posts: { path: string; body: Json }[] = []
  await page.route('**/admin/dlq*', (route) => {
    if (route.request().method() !== 'GET') return route.fallback()
    return route.fulfill({
      json: {
        pending: 1,
        data: [
          {
            id: 11,
            source: 'chsink',
            error: 'clickhouse timeout',
            retry_count: 3,
            status: 0,
            created_at: '2026-09-08T00:00:00Z',
            resolved_at: null,
            resolved_by: null,
            request_id: 'req-pending',
            user_id: 7,
            model: 'gpt-5',
            amount_micro: 1_000_000,
          },
          {
            id: 12,
            source: 'chsink',
            error: 'poison',
            retry_count: 8,
            status: 2,
            created_at: '2026-09-07T00:00:00Z',
            resolved_at: '2026-09-07T01:00:00Z',
            resolved_by: 1,
            request_id: 'req-done',
            user_id: 8,
            model: 'gpt-4o',
            amount_micro: 500_000,
          },
        ],
      },
    })
  })
  await page.route('**/admin/dlq/requeue', async (route) => {
    posts.push({ path: '/admin/dlq/requeue', body: route.request().postDataJSON() as Json })
    await route.fulfill({ json: { requeued: 1 } })
  })
  await page.route('**/admin/dlq/discard', async (route) => {
    posts.push({ path: '/admin/dlq/discard', body: route.request().postDataJSON() as Json })
    await route.fulfill({ json: { discarded: 1 } })
  })

  await page.goto('/admin/ops')
  await page.getByRole('tab', { name: '死信队列', exact: true }).click()
  await expect(page.getByText('1 条待处理')).toBeVisible()
  await expect(page.getByRole('button', { name: '重投 (0)' })).toBeDisabled()
  await expect(page.getByRole('checkbox', { name: '#12' })).toHaveCount(0)

  await page.getByRole('checkbox', { name: '全选待处理' }).check()
  await page.getByRole('button', { name: '重投 (1)' }).click()
  const requeue = page.getByRole('alertdialog')
  await expect(requeue).toContainText('将 1 条死信')
  await requeue.getByRole('button', { name: '重投', exact: true }).click()
  await expect.poll(() => posts.length).toBe(1)
  expect(posts[0]).toEqual({ path: '/admin/dlq/requeue', body: { ids: [11] } })
  await expect(page.getByRole('status').filter({ hasText: '已重投 1 条' })).toBeVisible()

  await page.route('**/admin/dlq/requeue', (route) => route.fulfill(apiError(500, 'internal_error')))
  await page.getByRole('checkbox', { name: '#11' }).check()
  await page.getByRole('button', { name: '重投 (1)' }).click()
  await page.getByRole('alertdialog').getByRole('button', { name: '重投', exact: true }).click()
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误' })).toBeVisible()
  await page.getByRole('alert').getByRole('button', { name: '关闭', exact: true }).click()
  await page.route('**/admin/dlq/requeue', async (route) => {
    posts.push({ path: '/admin/dlq/requeue', body: route.request().postDataJSON() as Json })
    await route.fulfill({ json: { requeued: 1 } })
  })

  await page.getByRole('checkbox', { name: '#11' }).check()
  await page.route('**/admin/dlq/discard', (route) => route.fulfill(apiError(500, 'internal_error')))
  await page.getByRole('button', { name: '丢弃 (1)' }).click()
  await page.getByRole('alertdialog').getByRole('button', { name: '丢弃', exact: true }).click()
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误' })).toBeVisible()
  await page.getByRole('alert').getByRole('button', { name: '关闭', exact: true }).click()
  await page.route('**/admin/dlq/discard', async (route) => {
    posts.push({ path: '/admin/dlq/discard', body: route.request().postDataJSON() as Json })
    await route.fulfill({ json: { discarded: 1 } })
  })
  await page.getByRole('button', { name: '丢弃 (1)' }).click()
  const discard = page.getByRole('alertdialog')
  await expect(discard).toContainText('永久缺席统计')
  await discard.getByRole('button', { name: '丢弃', exact: true }).click()
  await expect.poll(() => posts.length).toBe(2)
  expect(posts[1]).toEqual({ path: '/admin/dlq/discard', body: { ids: [11] } })

  await page.route('**/admin/dlq*', (route) => {
    if (route.request().method() !== 'GET') return route.fallback()
    return route.fulfill({ json: { pending: 0, data: [] } })
  })
  await page.reload()
  await page.getByRole('tab', { name: '死信队列', exact: true }).click()
  await expect(page.getByText('没有死信。统计与账本一致。')).toBeVisible()

  await page.route('**/admin/dlq*', (route) => {
    if (route.request().isNavigationRequest() || route.request().method() !== 'GET') return route.fallback()
    return route.fulfill(apiError(500, 'internal_error'))
  })
  await page.reload()
  await page.getByRole('tab', { name: '死信队列', exact: true }).click()
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误，请稍后再试' })).toBeVisible()
  await expect(page.getByRole('button', { name: '重试' })).toHaveCount(0)
})

test('充值兑换卡：空码禁提交、首尾空白会 trim、入账 micro 与套餐字段、错码错误码文案', async ({ page }) => {
  await prepare(page)
  const codes: Json[] = []
  let reply: Json | ReturnType<typeof apiError> = {
    amount_micro: 1_230_000,
    balance_after_micro: 11_230_000,
    plan_code: 'starter',
    granted_group: 'vip',
    balance_valid_days: 30,
  }
  await page.route('**/api/me/redeem', async (route) => {
    codes.push(route.request().postDataJSON() as Json)
    if ('status' in reply) await route.fulfill(reply)
    else await route.fulfill({ json: reply })
  })

  await page.goto('/portal/topup')
  const submit = page.getByRole('button', { name: '核销', exact: true })
  await expect(submit).toBeDisabled()
  await page.locator('#code').fill('  OKAPI-AAAA-1111  ')
  await submit.click()
  await expect.poll(() => codes.length).toBe(1)
  expect(codes[0]).toEqual({ code: 'OKAPI-AAAA-1111' })
  await expect(page.getByRole('status').filter({ hasText: '套餐' })).toBeVisible()
  await expect(page.getByText('starter')).toBeVisible()
  await expect(page.getByText('vip')).toBeVisible()
  await expect(page.getByText('余额有效期 30 天')).toBeVisible()

  reply = apiError(400, 'redemption_invalid')
  await page.locator('#code').fill('nope')
  await submit.click()
  await expect.poll(() => codes.length).toBe(2)
  await expect(page.getByText('兑换码无效、已被使用或不属于你')).toBeVisible()

  reply = apiError(500, 'internal_error')
  await submit.click()
  await expect(page.getByText('服务内部错误')).toBeVisible()
})

test('运维退款 / 对账 / 保留：先查后退、单用户与全部校准体形状、缩短保留期二次确认', async ({ page }) => {
  await prepare(page)
  const calls: { path: string; method: string; body: Json | null }[] = []
  const rid = 'aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee'
  await page.route(`**/admin/billing/record/${rid}`, (route) =>
    route.fulfill({
      json: {
        request_id: rid,
        user_id: 7,
        username: 'alice',
        model: 'gpt-5',
        status: 20,
        amount_micro: 1_230_000,
        prompt_tokens: 10,
        completion_tokens: 20,
        error_code: null,
        created_at: '2026-09-08T00:00:00Z',
        refundable: true,
      },
    }),
  )
  await page.route('**/admin/billing/refund', async (route) => {
    calls.push({
      path: '/admin/billing/refund',
      method: 'POST',
      body: route.request().postDataJSON() as Json,
    })
    await route.fulfill({
      json: { outcome: 'refunded', refunded_micro: 1_230_000, balance_after_micro: 8_770_000 },
    })
  })
  await page.route('**/admin/reconciliation', (route) =>
    route.request().method() === 'GET'
      ? route.fulfill({
          json: {
            drift_count: 1,
            drifts: [
              {
                user_id: 7,
                username: 'alice',
                events_sum_micro: 5_000_000,
                redis_effective_micro: 4_000_000,
                pg_snapshot_micro: 4_000_000,
              },
            ],
          },
        })
      : route.fallback(),
  )
  await page.route('**/admin/reconciliation/repair', async (route) => {
    calls.push({
      path: '/admin/reconciliation/repair',
      method: 'POST',
      body: route.request().postDataJSON() as Json,
    })
    await route.fulfill({ json: { repaired: 1 } })
  })
  await page.route('**/admin/cache/flush', async (route) => {
    calls.push({ path: '/admin/cache/flush', method: 'POST', body: route.request().postDataJSON() as Json })
    await route.fulfill({ json: { ok: true } })
  })
  await page.route('**/admin/settings/retention_months', (route) =>
    route.fulfill({ json: { value: 12 } }),
  )
  await page.route('**/admin/settings', async (route) => {
    if (route.request().method() !== 'POST') return route.fallback()
    calls.push({
      path: '/admin/settings',
      method: 'POST',
      body: route.request().postDataJSON() as Json,
    })
    await route.fulfill({ json: { ok: true } })
  })

  await page.goto('/admin/ops')
  await expect(page.getByRole('button', { name: '查询这笔账' })).toBeDisabled()
  await page.locator('#rid').fill(rid)
  await page.getByRole('button', { name: '查询这笔账' }).click()
  await expect(page.getByText('已扣费，可退款')).toBeVisible()
  await page.locator('#rreason').fill('  ticket 42  ')
  await page.route('**/admin/billing/refund', (route) => route.fulfill(apiError(500, 'internal_error')))
  await page.getByRole('button', { name: '退款', exact: true }).click()
  await page.getByRole('alertdialog').getByRole('button', { name: '退款', exact: true }).click()
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误' })).toBeVisible()
  await page.getByRole('alert').getByRole('button', { name: '关闭', exact: true }).click()
  await page.route('**/admin/billing/refund', async (route) => {
    calls.push({
      path: '/admin/billing/refund',
      method: 'POST',
      body: route.request().postDataJSON() as Json,
    })
    await route.fulfill({
      json: { outcome: 'refunded', refunded_micro: 1_230_000, balance_after_micro: 8_770_000 },
    })
  })
  await page.getByRole('button', { name: '退款', exact: true }).click()
  const refund = page.getByRole('alertdialog')
  await expect(refund).toContainText('alice')
  await refund.getByRole('button', { name: '退款', exact: true }).click()
  await expect.poll(() => calls.some((c) => c.path === '/admin/billing/refund')).toBe(true)
  expect(calls.find((c) => c.path === '/admin/billing/refund')?.body).toEqual({
    request_id: rid,
    reason: 'ticket 42',
  })

  await page.getByRole('tab', { name: '三方对账', exact: true }).click()
  await page.getByRole('button', { name: '清理缓存', exact: true }).click()
  await expect.poll(() => calls.some((c) => c.path === '/admin/cache/flush')).toBe(true)

  await page.route('**/admin/cache/flush', (route) => route.fulfill(apiError(500, 'internal_error')))
  await page.getByRole('button', { name: '清理缓存', exact: true }).click()
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误' })).toBeVisible()
  await page.getByRole('alert').getByRole('button', { name: '关闭', exact: true }).click()
  await page.route('**/admin/cache/flush', async (route) => {
    calls.push({ path: '/admin/cache/flush', method: 'POST', body: route.request().postDataJSON() as Json })
    await route.fulfill({ json: { ok: true } })
  })
  await page.getByRole('row').filter({ hasText: 'alice' }).getByRole('button', { name: '按账本校准', exact: true }).click()
  await expect.poll(() => calls.filter((c) => c.path === '/admin/reconciliation/repair').length).toBe(1)
  expect(calls.find((c) => c.path === '/admin/reconciliation/repair')?.body).toEqual({ user_id: 7 })
  await dismissToasts(page)
  await page.route('**/admin/reconciliation/repair', (route) => route.fulfill(apiError(500, 'internal_error')))
  await page.getByRole('row').filter({ hasText: 'alice' }).getByRole('button', { name: '按账本校准', exact: true }).click()
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误' })).toBeVisible()
  await page.getByRole('alert').getByRole('button', { name: '关闭', exact: true }).click()
  await page.route('**/admin/reconciliation/repair', async (route) => {
    calls.push({
      path: '/admin/reconciliation/repair',
      method: 'POST',
      body: route.request().postDataJSON() as Json,
    })
    await route.fulfill({ json: { repaired: 1 } })
  })
  await page.getByRole('button', { name: '全部校准', exact: true }).click()
  const all = page.getByRole('alertdialog')
  await expect(all).toContainText('1 个用户')
  await all.getByRole('button', { name: '按账本校准', exact: true }).click()
  await expect.poll(() => calls.filter((c) => c.path === '/admin/reconciliation/repair').length).toBe(2)
  expect(calls.filter((c) => c.path === '/admin/reconciliation/repair').at(-1)?.body).toEqual({
    all: true,
    limit: 100000,
  })

  await page.getByRole('tab', { name: '数据保留策略', exact: true }).click()
  await page.locator('#retention').fill('24')
  await page.getByRole('button', { name: '保存', exact: true }).click()
  await expect.poll(() => calls.filter((c) => c.path === '/admin/settings').length).toBe(1)
  expect(calls.find((c) => c.path === '/admin/settings')?.body).toEqual({
    key: 'retention_months',
    value: 24,
  })
  await page.locator('#retention').fill('6')
  await page.getByRole('button', { name: '保存', exact: true }).click()
  const shrink = page.getByRole('alertdialog')
  await expect(shrink).toContainText('缩短')
  await shrink.getByRole('button', { name: '保存', exact: true }).click()
  await expect.poll(() => calls.filter((c) => c.path === '/admin/settings').length).toBe(2)
  expect(calls.filter((c) => c.path === '/admin/settings').at(-1)?.body).toEqual({
    key: 'retention_months',
    value: 6,
  })

  await page.route('**/admin/settings', (route) => {
    if (route.request().method() !== 'POST') return route.fallback()
    return route.fulfill(apiError(500, 'internal_error'))
  })
  await page.locator('#retention').fill('12')
  await page.getByRole('button', { name: '保存', exact: true }).click()
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误' })).toBeVisible()
  await page.getByRole('alert').getByRole('button', { name: '关闭', exact: true }).click()

  await page.route('**/admin/reconciliation', (route) => {
    if (route.request().isNavigationRequest() || route.request().method() !== 'GET') return route.fallback()
    return route.fulfill({ json: { drift_count: 0, drifts: [] } })
  })
  await page.reload()
  await page.getByRole('tab', { name: '三方对账', exact: true }).click()
  await expect(page.getByText('对账零差异：账本、线上余额、展示快照三方一致。')).toBeVisible()

  await page.route('**/admin/reconciliation', (route) => {
    if (route.request().isNavigationRequest() || route.request().method() !== 'GET') return route.fallback()
    return route.fulfill(apiError(500, 'internal_error'))
  })
  await page.reload()
  await page.getByRole('tab', { name: '三方对账', exact: true }).click()
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误，请稍后再试' })).toBeVisible()
  await expect(page.getByRole('button', { name: '重试' })).toHaveCount(0)
})

test('注册表单：逐字输入小数、最小单位和大金额精确往返，切签保留草稿及未知配置', async ({ page }) => {
  await prepare(page)
  const value = { mode: 'invite_only', email_domain_mode: 'allowlist', email_domains: ['Example.COM'], new_user_credit_micro: 1,
    invitee_credit_micro: 0, inviter_credit_micro: Number.MAX_SAFE_INTEGER, email_verification: true, extension: { keep: true } }
  const posts: Json[] = []
  let reads = 0
  await page.route('**/admin/settings/registration_policy', (route) => { reads++; return route.fulfill({ json: { value } }) })
  await page.route('**/admin/settings', (route) => {
    if (route.request().method() !== 'POST') return route.fallback()
    posts.push(route.request().postDataJSON())
    return route.fulfill({ json: { ok: true } })
  })
  await page.goto('/admin/settings')
  const gift = page.locator('#reg-gift')
  await expect(gift).toHaveValue('0.000001')
  await expect(page.locator('#reg-inviter')).toHaveValue('9007199254.740991')
  await gift.fill('')
  await gift.pressSequentially('0.')
  await expect(gift).toHaveValue('0.')
  await expect(gift).toHaveAttribute('aria-invalid', 'false')
  await gift.pressSequentially('29')
  await expect(gift).toHaveValue('0.29')
  await page.locator('#reg-invitee').fill('.000001')
  await page.getByRole('tab', { name: '站点公告', exact: true }).click()
  await page.getByRole('tab', { name: '注册与风控', exact: true }).click()
  await expect(gift).toHaveValue('0.29')
  await expect(page.locator('#reg-invitee')).toHaveValue('.000001')
  await gift.press('Enter')
  await expect.poll(() => posts.length).toBe(1)
  expect(posts[0]).toEqual({ key: 'registration_policy', value: { ...value, email_domains: ['example.com'], new_user_credit_micro: 290000, invitee_credit_micro: 1 } })
  await expect(page.getByRole('button', { name: '保存', exact: true })).toBeDisabled()
  await expect(gift).toHaveValue('0.29')
  await expect(page.locator('#reg-invitee')).toHaveValue('0.000001')
  await expect(page.locator('#reg-inviter')).toHaveValue('9007199254.740991')
  expect(reads).toBe(1)
})

test('注册表单：非法金额保留原文并定位错误，不能隐式提交或悄悄变成零', async ({ page }) => {
  await prepare(page)
  const posts: Json[] = []
  await page.route('**/admin/settings', (route) => {
    if (route.request().method() !== 'POST') return route.fallback()
    posts.push(route.request().postDataJSON())
    return route.fulfill({ json: { ok: true } })
  })
  await page.goto('/admin/settings')
  const gift = page.locator('#reg-gift')
  const save = page.getByRole('button', { name: '保存', exact: true })
  for (const raw of ['-1', 'abc', '1e3', '0x10', '1,2', '1.0000001', '1.2.', '.', '9007199254.740992']) {
    await gift.fill(raw)
    await expect(gift).toHaveValue(raw)
    await expect(gift).toHaveAttribute('aria-invalid', 'true')
    await expect(gift).toHaveAccessibleDescription(raw === '9007199254.740992' ? /金额不能超过/ : /请输入不小于 0/)
    await expect(save).toBeDisabled()
    await gift.press('Enter')
  }
  expect(posts).toHaveLength(0)
  await gift.fill('1.')
  await expect(gift).toHaveValue('1.')
  await expect(gift).toHaveAttribute('aria-invalid', 'false')
  await page.locator('#reg-invitee').fill('-2')
  await expect(save).toBeDisabled()
  await page.locator('#reg-invitee').fill('')
  await page.locator('#reg-inviter').fill('3.1234567')
  await expect(save).toBeDisabled()
  await page.locator('#reg-inviter').fill('0')
  await save.click()
  await expect.poll(() => posts.length).toBe(1)
  expect(posts[0].value).toMatchObject({ new_user_credit_micro: 1000000, invitee_credit_micro: 0, inviter_credit_micro: 0 })
})

test('注册表单：配置读取完成前和失败后不可编辑默认值，重试恢复已有策略', async ({ page }) => {
  await prepare(page)
  let release: () => void = () => undefined
  const pending = new Promise<void>((resolve) => { release = resolve })
  await page.route('**/admin/settings/registration_policy', async (route) => {
    await pending
    await route.fulfill(apiError(500, 'internal_error'))
  })
  await page.goto('/admin/settings')
  const panel = page.getByRole('tabpanel', { name: '注册与风控' })
  try {
    await expect(panel.getByRole('status')).toBeVisible()
    await expect(panel.getByRole('textbox')).toHaveCount(0)
    await expect(panel.getByRole('button', { name: '保存', exact: true })).toHaveCount(0)
  } finally { release() }
  await expect(panel.getByRole('alert')).toBeVisible()
  await expect(panel.getByRole('textbox')).toHaveCount(0)
  await page.route('**/admin/settings/registration_policy', (route) => route.fulfill({ json: { value: { mode: 'closed', new_user_credit_micro: 12500000, email_verification: true } } }))
  await panel.getByRole('button', { name: '重试' }).click()
  await expect(panel.getByRole('group', { name: '注册方式' }).getByRole('button', { name: '关闭', exact: true })).toHaveAttribute('aria-pressed', 'true')
  await expect(page.locator('#reg-gift')).toHaveValue('12.5')
  await expect(panel.getByRole('switch')).toBeChecked()
  await expect(panel.getByRole('button', { name: '保存', exact: true })).toBeDisabled()
})

test('注册表单：保存中锁定操作，失败保留原始草稿，重试成功后取消恢复最新保存值', async ({ page }) => {
  await prepare(page)
  const posts: Json[] = []
  let release: () => void = () => undefined
  const pending = new Promise<void>((resolve) => { release = resolve })
  await page.route('**/admin/settings', async (route) => {
    if (route.request().method() !== 'POST') return route.fallback()
    posts.push(route.request().postDataJSON())
    if (posts.length === 1) { await pending; await route.fulfill(apiError(500, 'internal_error')) }
    else await route.fulfill({ json: { ok: true } })
  })
  await page.goto('/admin/settings')
  const panel = page.getByRole('tabpanel', { name: '注册与风控' })
  const gift = page.locator('#reg-gift')
  await gift.fill('01.20')
  await panel.getByRole('button', { name: '仅允许', exact: true }).click()
  await page.locator('#reg-domains').fill('Example.COM, *.edu.cn\nexample.com，mail.test')
  await panel.getByRole('button', { name: '保存', exact: true }).click()
  try {
    await expect.poll(() => posts.length).toBe(1)
    expect(posts[0].value).toMatchObject({ new_user_credit_micro: 1200000, email_domains: ['example.com', '*.edu.cn', 'mail.test'] })
    for (const id of ['reg-gift', 'reg-invitee', 'reg-inviter', 'reg-domains']) await expect(page.locator(`#${id}`)).toBeDisabled()
    await expect(panel.getByRole('button', { name: '取消', exact: true })).toBeDisabled()
    await expect(panel.getByRole('button', { name: '开放', exact: true })).toBeDisabled()
    await expect(panel.getByRole('switch')).toBeDisabled()
    await expect(panel.getByRole('button', { name: '保存', exact: true })).toHaveAttribute('aria-busy', 'true')
    await page.getByRole('tab', { name: '站点公告', exact: true }).click()
    await page.getByRole('tab', { name: '注册与风控', exact: true }).click()
    await expect(gift).toHaveValue('01.20')
    await expect(gift).toBeDisabled()
  } finally { release() }
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误' })).toBeVisible()
  await expect(gift).toBeEnabled()
  await expect(gift).toHaveValue('01.20')
  await expect(page.locator('#reg-domains')).toHaveValue('Example.COM, *.edu.cn\nexample.com，mail.test')
  await dismissToasts(page)
  await panel.getByRole('button', { name: '保存', exact: true }).click()
  await expect.poll(() => posts.length).toBe(2)
  expect(posts[1]).toEqual(posts[0])
  await expect(gift).toHaveValue('1.2')
  await expect(page.locator('#reg-domains')).toHaveValue('example.com\n*.edu.cn\nmail.test')
  await gift.fill('2.')
  await page.locator('#reg-domains').fill('other.test')
  await panel.getByRole('button', { name: '取消', exact: true }).click()
  await expect(gift).toHaveValue('1.2')
  await expect(page.locator('#reg-domains')).toHaveValue('example.com\n*.edu.cn\nmail.test')
  await expect(panel.getByRole('button', { name: '保存', exact: true })).toBeDisabled()
})

for (const width of [320, 390, 1280]) {
  test(`注册表单：${width}px 长金额与错误说明可读，控件不超出视口`, async ({ page }) => {
    await prepare(page)
    if (width === 390) await page.addInitScript(() => localStorage.setItem('okapi.lang', 'en'))
    await page.setViewportSize({ width, height: 800 })
    await page.goto('/admin/settings')
    await page.getByRole('button', { name: width === 390 ? 'Allow only' : '仅允许', exact: true }).click()
    await page.locator('#reg-domains').fill('a-very-long-domain-name.example.com\n*.education.example.com')
    await page.locator('#reg-gift').fill('9007199254.740992')
    await page.locator('#reg-invitee').fill('9007199254.740991')
    await page.locator('#reg-inviter').fill('1.1234567')
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    for (const id of ['reg-gift', 'reg-invitee', 'reg-inviter']) {
      const box = await page.locator(`#${id}`).boundingBox()
      expect(box!.x).toBeGreaterThanOrEqual(0)
      expect(box!.x + box!.width).toBeLessThanOrEqual(width)
      if (width < 768) expect(box!.height).toBeGreaterThanOrEqual(44)
    }
    if (width === 390) await page.evaluate(() => document.documentElement.classList.add('dark'))
    await page.locator('#main-content').getByRole('heading', { level: 1 }).scrollIntoViewIfNeeded()
    await page.screenshot({ path: `test-results/registration-${width}.png`, fullPage: true, animations: 'disabled' })
  })
}

test('设置：公告发布带 updated_at、注册赠送 USD→micro、隐私开关即时 POST、MCP 写入走高级设置抽屉', async ({
  page,
}) => {
  await prepare(page)
  const posts: Json[] = []
  await page.route('**/admin/settings', async (route) => {
    if (route.request().isNavigationRequest()) return route.fallback()
    if (route.request().method() === 'POST') {
      posts.push(route.request().postDataJSON() as Json)
      await route.fulfill({ json: { ok: true } })
      return
    }
    await route.fulfill({
      json: {
        data: [
          {
            key: 'mcp_write_enabled',
            value: false,
            is_secret: false,
            configured: true,
            updated_at: null,
          },
        ],
      },
    })
  })

  await page.goto('/admin/settings')
  await page.getByRole('button', { name: '仅邀请', exact: true }).click()
  await page.locator('#reg-gift').fill('0.29')
  await page.route('**/admin/settings', (route) => {
    if (route.request().isNavigationRequest() || route.request().method() !== 'POST') return route.fallback()
    return route.fulfill(apiError(500, 'internal_error'))
  })
  await page.getByRole('button', { name: '保存', exact: true }).click()
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误' })).toBeVisible()
  await page.getByRole('alert').getByRole('button', { name: '关闭', exact: true }).click()
  await page.route('**/admin/settings', async (route) => {
    if (route.request().isNavigationRequest()) return route.fallback()
    if (route.request().method() === 'POST') {
      posts.push(route.request().postDataJSON() as Json)
      await route.fulfill({ json: { ok: true } })
      return
    }
    await route.fulfill({
      json: {
        data: [
          {
            key: 'mcp_write_enabled',
            value: false,
            is_secret: false,
            configured: true,
            updated_at: null,
          },
        ],
      },
    })
  })
  await page.getByRole('button', { name: '保存', exact: true }).click()
  await expect.poll(() => posts.length).toBe(1)
  expect(posts[0]).toEqual({
    key: 'registration_policy',
    value: {
      mode: 'invite_only',
      email_domain_mode: 'any',
      email_domains: [],
      new_user_credit_micro: 290_000,
      invitee_credit_micro: 0,
      inviter_credit_micro: 0,
      email_verification: false,
    },
  })

  await page.getByRole('tab', { name: '站点公告', exact: true }).click()
  await page.getByRole('switch', { name: '启用公告' }).click()
  await page.locator('#notice-title').fill('维护窗口')
  await page.locator('#notice-level').selectOption('warning')
  await page.locator('#notice-body').fill('今晚 02:00 升级')
  await page.getByRole('button', { name: '保存并发布', exact: true }).click()
  await expect.poll(() => posts.length).toBe(2)
  const notice = posts[1] as { key: string; value: Json }
  expect(notice.key).toBe('site_notice')
  expect(notice.value).toMatchObject({
    enabled: true,
    title: '维护窗口',
    body: '今晚 02:00 升级',
    level: 'warning',
  })
  expect(typeof notice.value.updated_at).toBe('string')
  expect(notice.value.updated_at).toMatch(/^\d{4}-\d{2}-\d{2}T/)
  await expect(page.getByRole('button', { name: '保存并发布', exact: true })).toBeDisabled()
  await dismissToasts(page)

  await page.locator('#notice-title').fill('维护窗口 2')
  await expect(page.getByRole('button', { name: '保存并发布', exact: true })).toBeEnabled()
  await page.route('**/admin/settings', (route) => {
    if (route.request().isNavigationRequest() || route.request().method() !== 'POST') return route.fallback()
    return route.fulfill(apiError(500, 'internal_error'))
  })
  await page.getByRole('button', { name: '保存并发布', exact: true }).click()
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误' })).toBeVisible()
  await page.getByRole('alert').getByRole('button', { name: '关闭', exact: true }).click()
  await page.route('**/admin/settings', async (route) => {
    if (route.request().isNavigationRequest()) return route.fallback()
    if (route.request().method() === 'POST') {
      posts.push(route.request().postDataJSON() as Json)
      await route.fulfill({ json: { ok: true } })
      return
    }
    await route.fulfill({
      json: {
        data: [
          {
            key: 'mcp_write_enabled',
            value: false,
            is_secret: false,
            configured: true,
            updated_at: null,
          },
        ],
      },
    })
  })

  await page.getByRole('tab', { name: '隐私与留痕', exact: true }).click()
  await page.getByRole('switch', { name: '记录请求来源 IP' }).click()
  await expect.poll(() => posts.length).toBe(3)
  expect(posts[2]).toEqual({ key: 'record_ip_log', value: false })
  await dismissToasts(page)
  await page.route('**/admin/settings', (route) => {
    if (route.request().isNavigationRequest() || route.request().method() !== 'POST') return route.fallback()
    return route.fulfill(apiError(500, 'internal_error'))
  })
  await page.getByRole('switch', { name: '记录请求来源 IP' }).click()
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误' })).toBeVisible()
  await page.getByRole('alert').getByRole('button', { name: '关闭', exact: true }).click()
  await page.route('**/admin/settings', async (route) => {
    if (route.request().isNavigationRequest()) return route.fallback()
    if (route.request().method() === 'POST') {
      posts.push(route.request().postDataJSON() as Json)
      await route.fulfill({ json: { ok: true } })
      return
    }
    await route.fulfill({
      json: {
        data: [
          {
            key: 'mcp_write_enabled',
            value: false,
            is_secret: false,
            configured: true,
            updated_at: null,
          },
        ],
      },
    })
  })

  await page.getByRole('tab', { name: '高级设置', exact: true }).click()
  await page.getByRole('button', { name: '配置 MCP 写入权限' }).click()
  const drawer = await openedDialog(page)
  await drawer.getByRole('switch', { name: 'MCP 写入权限' }).click()
  await page.route('**/admin/settings', (route) => {
    if (route.request().isNavigationRequest() || route.request().method() !== 'POST') return route.fallback()
    return route.fulfill(apiError(500, 'internal_error'))
  })
  await drawer.getByRole('button', { name: '保存', exact: true }).click()
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误' })).toBeVisible()
  await page.getByRole('alert').getByRole('button', { name: '关闭', exact: true }).click()
  await page.route('**/admin/settings', async (route) => {
    if (route.request().isNavigationRequest()) return route.fallback()
    if (route.request().method() === 'POST') {
      posts.push(route.request().postDataJSON() as Json)
      await route.fulfill({ json: { ok: true } })
      return
    }
    await route.fulfill({
      json: {
        data: [
          {
            key: 'mcp_write_enabled',
            value: false,
            is_secret: false,
            configured: true,
            updated_at: null,
          },
        ],
      },
    })
  })
  await drawer.getByRole('button', { name: '保存', exact: true }).click()
  await expect.poll(() => posts.length).toBe(4)
  expect(posts[3]).toEqual({ key: 'mcp_write_enabled', value: true })

  await page.route('**/admin/settings', (route) => {
    if (route.request().isNavigationRequest() || route.request().method() !== 'GET') return route.fallback()
    return route.fulfill(apiError(500, 'internal_error'))
  })
  await page.reload()
  await page.getByRole('tab', { name: '高级设置', exact: true }).click()
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误，请稍后再试' })).toBeVisible()
  await expect(page.getByRole('button', { name: '重试' })).toBeVisible()
})

test('门户密钥：新建 POST /auth/keys 带分组与 IP；停用 PATCH status；删除手输名称', async ({ page }) => {
  await prepare(page)
  const calls: { path: string; method: string; body: Json | null }[] = []
  const keyRow = {
    id: 9,
    name: 'laptop',
    key_prefix: 'sk-okapi-abcd',
    status: 1,
    used_micro: 1_230_000,
    rpm_limit: 60,
    created_at: '2026-09-01T12:00:00Z',
    amount_micro: 1_230_000,
    requests: 12,
    group_override: 'vip',
    ip_allowlist: ['10.0.0.1/32'],
  }
  let mintOk = false
  await page.route('**/api/me/groups', (route) =>
    route.fulfill({
      json: {
        current: 'default',
        data: [
          { code: 'default', ratio: '1', description: null, source: 'default' },
          { code: 'vip', ratio: '0.5', description: 'VIP', source: 'assigned' },
        ],
      },
    }),
  )
  await page.route('**/api/me/keys*', (route) => {
    if (route.request().method() !== 'GET') return route.fallback()
    return route.fulfill({ json: { total: 1, data: [keyRow] } })
  })
  await page.route('**/auth/keys', async (route) => {
    if (route.request().method() !== 'POST') return route.fallback()
    if (!mintOk) return route.fulfill(apiError(401, 'unauthorized'))
    calls.push({
      path: '/auth/keys',
      method: 'POST',
      body: route.request().postDataJSON() as Json,
    })
    await route.fulfill({ json: { key_id: 10, api_key: 'sk-okapi-new-plain' } })
  })
  await page.route('**/api/me/keys/9', async (route) => {
    const body = (route.request().postDataJSON() ?? null) as Json | null
    calls.push({
      path: '/api/me/keys/9',
      method: route.request().method(),
      body,
    })
    if (route.request().method() === 'PATCH' && body && typeof body.status === 'number') {
      keyRow.status = body.status
    }
    await route.fulfill({ json: { ok: true } })
  })

  await page.goto('/portal/keys')
  const laptop = page.getByRole('row').filter({ hasText: 'laptop' })
  await expect(laptop.getByText('分组：vip')).toBeVisible()
  await expect(laptop.getByText('限 1 个来源')).toBeVisible()
  await expect(laptop.getByText(/\$1\.23/)).toBeVisible()
  await expect(laptop.getByText('60')).toBeVisible()

  await page.getByRole('button', { name: '新建密钥', exact: true }).click()
  const denied = await openedDialog(page)
  await denied.locator('#key-name').fill('session-key')
  await denied.getByRole('button', { name: '新建', exact: true }).click()
  await expect(page.getByText(/新建密钥需邮箱密码登录/)).toBeVisible()
  await expect(page.getByRole('dialog')).toHaveCount(0)
  mintOk = true

  await laptop.getByRole('button', { name: '编辑', exact: true }).click()
  const rename = await openedDialog(page)
  await expect(rename.getByRole('heading', { name: '编辑密钥' })).toBeVisible()
  await expect(rename.locator('#key-name')).toHaveValue('laptop')
  await expect(rename.locator('#key-group')).toHaveValue('vip')
  await rename.locator('#key-name').fill('   ')
  await expect(rename.getByRole('button', { name: '保存', exact: true })).toBeDisabled()
  await rename.locator('#key-name').fill('  desk  ')
  await rename.locator('#key-group').selectOption('')
  await rename.getByRole('button', { name: '移除 10.0.0.1/32' }).click()
  await rename.locator('#key-ips').fill('198.51.100.8')
  await rename.locator('#key-ips').press('Enter')
  await page.route('**/api/me/keys/9', (route) => {
    if (route.request().method() !== 'PATCH') return route.fallback()
    return route.fulfill(apiError(500, 'internal_error'))
  })
  await rename.getByRole('button', { name: '保存', exact: true }).click()
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误' })).toBeVisible()
  await page.getByRole('alert').getByRole('button', { name: '关闭', exact: true }).click()
  await page.route('**/api/me/keys/9', async (route) => {
    const body = (route.request().postDataJSON() ?? null) as Json | null
    calls.push({
      path: '/api/me/keys/9',
      method: route.request().method(),
      body,
    })
    if (route.request().method() === 'PATCH' && body && typeof body.status === 'number') {
      keyRow.status = body.status
    }
    await route.fulfill({ json: { ok: true } })
  })
  await rename.getByRole('button', { name: '保存', exact: true }).click()
  await expect.poll(() => calls).toEqual([
    {
      path: '/api/me/keys/9',
      method: 'PATCH',
      body: { name: 'desk', group_code: null, ip_allowlist: ['198.51.100.8'] },
    },
  ])
  await expect(page.getByRole('status').filter({ hasText: '已保存' })).toBeVisible()
  await dismissToasts(page)

  await page.getByRole('button', { name: '新建密钥', exact: true }).click()
  const drawer = await openedDialog(page)
  const create = drawer.getByRole('button', { name: '新建', exact: true })
  await expect(create).toBeDisabled()
  await drawer.locator('#key-name').fill('  ci-bot  ')
  await drawer.locator('#key-group').selectOption('vip')
  await drawer.locator('#key-ips').fill('198.51.100.1')
  await drawer.locator('#key-ips').press('Enter')
  await page.route('**/auth/keys', (route) => {
    if (route.request().method() !== 'POST') return route.fallback()
    return route.fulfill(apiError(500, 'internal_error'))
  })
  await create.click()
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误' })).toBeVisible()
  await page.getByRole('alert').getByRole('button', { name: '关闭', exact: true }).click()
  await dismissToasts(page)
  await page.route('**/auth/keys', async (route) => {
    if (route.request().method() !== 'POST') return route.fallback()
    calls.push({
      path: '/auth/keys',
      method: 'POST',
      body: route.request().postDataJSON() as Json,
    })
    await route.fulfill({ json: { key_id: 10, api_key: 'sk-okapi-new-plain' } })
  })
  await create.click()
  await expect.poll(() => calls.length).toBe(2)
  expect(calls[0]).toEqual({
    path: '/api/me/keys/9',
    method: 'PATCH',
    body: { name: 'desk', group_code: null, ip_allowlist: ['198.51.100.8'] },
  })
  expect(calls[1]).toEqual({
    path: '/auth/keys',
    method: 'POST',
    body: { name: 'ci-bot', group_code: 'vip', ip_allowlist: ['198.51.100.1'] },
  })
  await expect(page.getByText(/密钥「ci-bot」已创建/)).toBeVisible()
  await expect(page.getByText('sk-okapi-new-plain')).toBeVisible()
  await page.context().grantPermissions(['clipboard-read', 'clipboard-write'])
  await page.getByRole('status').filter({ hasText: 'sk-okapi-new-plain' }).getByRole('button', { name: '复制', exact: true }).click()
  await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toBe('sk-okapi-new-plain')
  await dismissToasts(page)

  const row = page.getByRole('row').filter({ hasText: 'laptop' })
  await page.route('**/api/me/keys/9', (route) => {
    if (route.request().method() !== 'PATCH') return route.fallback()
    return route.fulfill(apiError(500, 'internal_error'))
  })
  await row.getByRole('button', { name: '停用', exact: true }).click()
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误' })).toBeVisible()
  await page.getByRole('alert').getByRole('button', { name: '关闭', exact: true }).click()
  await page.route('**/api/me/keys/9', async (route) => {
    const body = (route.request().postDataJSON() ?? null) as Json | null
    calls.push({
      path: '/api/me/keys/9',
      method: route.request().method(),
      body,
    })
    if (route.request().method() === 'PATCH' && body && typeof body.status === 'number') {
      keyRow.status = body.status
    }
    await route.fulfill({ json: { ok: true } })
  })
  await row.getByRole('button', { name: '停用', exact: true }).click()
  await expect.poll(() => calls.length).toBe(3)
  expect(calls[2]).toEqual({
    path: '/api/me/keys/9',
    method: 'PATCH',
    body: { status: 2 },
  })
  await dismissToasts(page)
  await row.getByRole('button', { name: '启用', exact: true }).click()
  await expect.poll(() => calls.length).toBe(4)
  expect(calls[3]).toEqual({
    path: '/api/me/keys/9',
    method: 'PATCH',
    body: { status: 1 },
  })

  await row.getByRole('button', { name: '删除', exact: true }).click()
  const confirm = page.getByRole('alertdialog')
  await expect(confirm.getByRole('button', { name: '删除', exact: true })).toBeDisabled()
  await confirm.locator('#confirm-text').fill('laptop')
  await page.route('**/api/me/keys/9', (route) => {
    if (route.request().method() !== 'DELETE') return route.fallback()
    return route.fulfill(apiError(500, 'internal_error'))
  })
  await confirm.getByRole('button', { name: '删除', exact: true }).click()
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误' })).toBeVisible()
  await page.getByRole('alert').getByRole('button', { name: '关闭', exact: true }).click()
  await page.route('**/api/me/keys/9', async (route) => {
    const body = (route.request().postDataJSON() ?? null) as Json | null
    calls.push({
      path: '/api/me/keys/9',
      method: route.request().method(),
      body,
    })
    if (route.request().method() === 'PATCH' && body && typeof body.status === 'number') {
      keyRow.status = body.status
    }
    await route.fulfill({ json: { ok: true } })
  })
  await row.getByRole('button', { name: '删除', exact: true }).click()
  const retry = page.getByRole('alertdialog')
  await retry.locator('#confirm-text').fill('laptop')
  await retry.getByRole('button', { name: '删除', exact: true }).click()
  await expect.poll(() => calls.some((c) => c.method === 'DELETE')).toBe(true)
  expect(calls.find((c) => c.method === 'DELETE')).toEqual({
    path: '/api/me/keys/9',
    method: 'DELETE',
    body: null,
  })

  await page.route('**/api/me/keys*', (route) => {
    if (route.request().method() !== 'GET') return route.fallback()
    return route.fulfill({ json: { total: 0, data: [] } })
  })
  await page.reload()
  await expect(page.getByText('还没有密钥。点右上角「新建密钥」（需邮箱密码登录）。')).toBeVisible()
  await page
    .getByText('还没有密钥。点右上角「新建密钥」（需邮箱密码登录）。')
    .locator('xpath=ancestor::div[contains(@class,"border-dashed")]')
    .getByRole('button', { name: '新建密钥' })
    .click()
  await expect(page.getByRole('dialog').getByRole('heading', { name: '新建密钥' })).toBeVisible()

  await page.route('**/api/me/keys*', (route) => {
    if (route.request().method() !== 'GET') return route.fallback()
    return route.fulfill(apiError(500, 'internal_error'))
  })
  await page.reload()
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误，请稍后再试' })).toBeVisible()
  await expect(page.getByRole('button', { name: '重试' })).toBeVisible()
})

async function prepareNotifications(page: Page, initial: unknown = []) {
  await prepare(page)
  const posts: Json[] = []
  let stored = initial
  let reads = 0
  await page.route('**/admin/settings/notify_channels', (route) => {
    reads++
    return route.fulfill({ json: { value: stored } })
  })
  await page.route('**/admin/settings', async (route) => {
    if (route.request().isNavigationRequest() || route.request().method() !== 'POST') return route.fallback()
    const body = route.request().postDataJSON() as Json
    posts.push(body)
    stored = body.value
    await route.fulfill({ json: { ok: true } })
  })
  return {
    posts, reads: () => reads,
    panel: page.getByRole('tabpanel', { name: '通知多路', exact: true }),
    open: async () => {
      await page.goto('/admin/settings')
      await page.getByRole('tab', { name: '通知多路', exact: true }).click()
    },
  }
}

test('通知配置：加载、读取失败与空配置分开，格式异常不能保存成空列表', async ({ page }) => {
  const { open, panel, posts } = await prepareNotifications(page)
  let release: () => void = () => undefined
  const pending = new Promise<void>((resolve) => { release = resolve })
  await page.route('**/admin/settings/notify_channels', async (route) => {
    await pending
    await route.fulfill(apiError(500, 'internal_error'))
  })
  await open()
  try {
    await expect(panel.getByRole('status')).toBeVisible()
    await expect(panel.getByRole('button', { name: '保存', exact: true })).toHaveCount(0)
    await expect(panel.getByRole('button', { name: '添加邮件', exact: true })).toHaveCount(0)
    await expect(panel.getByText('还没有通知渠道')).toHaveCount(0)
  } finally { release() }
  await expect(panel.getByRole('alert')).toContainText('服务内部错误')
  await page.route('**/admin/settings/notify_channels', (route) => route.fulfill({ json: { value: [{ type: 'email', events: 'broken' }] } }))
  await panel.getByRole('button', { name: '重试', exact: true }).click()
  await expect(panel.getByRole('alert')).toContainText('通知配置格式无法识别')
  await expect(panel.getByRole('button', { name: '保存', exact: true })).toHaveCount(0)
  await page.route('**/admin/settings/notify_channels', (route) => route.fulfill({ json: { value: [] } }))
  await panel.getByRole('button', { name: '重试', exact: true }).click()
  await expect(panel.getByText('还没有通知渠道')).toBeVisible()
  await expect(panel.getByRole('button', { name: '保存', exact: true })).toBeEnabled()
  expect(posts).toHaveLength(0)
})

test('通知配置：间隔保留原始输入，非法值定位到字段，留空使用默认值且保留扩展配置', async ({ page }) => {
  const channel = { type: 'webhook', url: 'https://hooks.example.com/okapi', events: ['drift', 'future_event'], min_interval_secs: 60, extension: { retained: true } }
  const { open, panel, posts } = await prepareNotifications(page, [channel])
  await open()
  const interval = panel.getByLabel('最小间隔（秒）', { exact: true })
  for (const raw of ['-1', '1.5', '1e3', 'abc', '9007199254740992']) {
    await interval.fill(raw)
    await expect(interval).toHaveValue(raw)
    await expect(interval).toHaveAttribute('aria-invalid', 'true')
    await panel.getByRole('button', { name: '保存', exact: true }).click()
    await expect(interval).toBeFocused()
    await expect(interval).toHaveAccessibleDescription(/有效的非负整数/)
  }
  expect(posts).toHaveLength(0)
  await interval.fill('')
  await expect(interval).toHaveValue('')
  await panel.getByRole('button', { name: '保存', exact: true }).click()
  await expect.poll(() => posts.length).toBe(1)
  expect(posts[0]).toEqual({ key: 'notify_channels', value: [{ ...channel, min_interval_secs: 300 }] })
  await expect(interval).toHaveValue('300')
  await interval.fill('0')
  await panel.getByRole('button', { name: '保存', exact: true }).click()
  await expect.poll(() => posts.length).toBe(2)
  expect(posts[1]).toEqual({ key: 'notify_channels', value: [{ ...channel, min_interval_secs: 0 }] })
})

for (const width of [320, 1280]) test(`通知配置 ${width}px：地址与邮箱错误就地提示，批量收件人直接保存，不选事件显示停发说明`, async ({ page }) => {
  await page.setViewportSize({ width, height: 800 })
  const { open, panel, posts } = await prepareNotifications(page)
  await open()
  await panel.getByRole('button', { name: '添加 Webhook', exact: true }).click()
  const url = panel.getByLabel('Webhook 地址', { exact: true })
  await url.fill('mailto:ops@example.com')
  await panel.getByRole('button', { name: '保存', exact: true }).click()
  await expect(url).toBeFocused()
  await expect(url).toHaveAttribute('aria-invalid', 'true')
  await expect(url).toHaveAccessibleDescription('请输入完整的 HTTP 或 HTTPS 地址。')
  await url.fill(' https://hooks.example.com/path?token=fixture ')
  await panel.getByRole('button', { name: '添加邮件', exact: true }).click()
  const email = panel.getByRole('region', { name: '邮件 2', exact: true })
  const input = email.getByLabel('收件人', { exact: true })
  await input.fill('not-an-address')
  await panel.getByRole('button', { name: '保存', exact: true }).click()
  await expect(input).toBeFocused()
  await expect(input).toHaveAttribute('aria-invalid', 'true')
  await expect(input).toHaveAccessibleDescription(/至少添加一个有效的邮箱地址/)
  expect(posts).toHaveLength(0)
  await email.getByRole('button', { name: '移除 not-an-address', exact: true }).click()
  await input.fill('one@example.com, two@example.com，one@example.com')
  // 不要求先按回车；保存按钮的点击不能被标签确认挤走。
  await panel.getByRole('button', { name: '保存', exact: true }).click()
  await expect.poll(() => posts.length).toBe(1)
  const values = posts[0]!.value as Json[]
  expect(values[0]!.url).toBe('https://hooks.example.com/path?token=fixture')
  expect(values[1]!.to).toEqual(['one@example.com', 'two@example.com'])
  await expect(input).toHaveValue('')
  for (const checkbox of await email.getByRole('checkbox').all()) await checkbox.uncheck()
  await expect(email.getByText('尚未选择事件，这一路不会发送通知。')).toBeVisible()
  await panel.getByRole('button', { name: '保存', exact: true }).click()
  await expect.poll(() => posts.length).toBe(2)
  expect((posts[1]!.value as Json[])[1]!.events).toEqual([])
})

test('通知配置：删除前一路保留下一路输入与语言，取消清理未确认收件人并恢复已保存值', async ({ page }) => {
  const stored = [
    { type: 'email', to: ['first@example.com'], lang: 'en', events: ['drift'], min_interval_secs: 60 },
    { type: 'email', to: ['second@example.com'], lang: 'zh-CN', events: ['balance_low'], min_interval_secs: 120 },
  ]
  const { open, panel, posts } = await prepareNotifications(page, stored)
  await open()
  await panel.getByRole('region', { name: '邮件 2', exact: true }).getByLabel('收件人', { exact: true }).fill('pending@example.com')
  await panel.getByRole('region', { name: '邮件 1', exact: true }).getByRole('button', { name: '删除', exact: true }).click()
  const remaining = panel.getByRole('region', { name: '邮件 1', exact: true })
  await expect(remaining.getByLabel('收件人', { exact: true })).toBeFocused()
  await expect(remaining.getByRole('button', { name: '移除 pending@example.com', exact: true })).toBeVisible()
  await expect(remaining.getByRole('button', { name: '移除 second@example.com', exact: true })).toBeVisible()
  await expect(remaining.getByRole('button', { name: '中文', exact: true })).toHaveAttribute('aria-pressed', 'true')
  await expect(remaining.getByLabel('最小间隔（秒）', { exact: true })).toHaveValue('120')
  await remaining.getByLabel('收件人', { exact: true }).fill('discard@example.com')
  await panel.getByRole('button', { name: '取消', exact: true }).click()
  await expect(panel.getByRole('region')).toHaveCount(2)
  await expect(panel.getByLabel('收件人', { exact: true }).first()).toHaveValue('')
  await expect(panel.getByRole('button', { name: /pending@example.com|discard@example.com/ })).toHaveCount(0)
  await expect(panel.getByRole('button', { name: '移除 first@example.com', exact: true })).toBeVisible()
  expect(posts).toHaveLength(0)
  await panel.getByRole('region', { name: '邮件 2', exact: true }).getByRole('button', { name: '删除', exact: true }).click()
  await panel.getByRole('region', { name: '邮件 1', exact: true }).getByRole('button', { name: '删除', exact: true }).click()
  await expect(panel.getByRole('button', { name: '添加 Webhook', exact: true })).toBeFocused()
})

test('通知配置：保存时锁定表单，失败保留草稿，成功回显快照并保留之后的编辑', async ({ page }) => {
  const { open, panel, posts, reads } = await prepareNotifications(page, [{ type: 'webhook', url: 'https://hooks.example.com/old', events: ['drift'], min_interval_secs: 60 }])
  let release: () => void = () => undefined
  const pending = new Promise<void>((resolve) => { release = resolve })
  let shouldFail = true
  let attempts = 0
  await page.route('**/admin/settings', async (route) => {
    if (route.request().isNavigationRequest() || route.request().method() !== 'POST') return route.fallback()
    attempts++
    if (!shouldFail) return route.fallback()
    await pending
    await route.fulfill(apiError(500, 'internal_error'))
  })
  await open()
  const url = panel.getByLabel('Webhook 地址', { exact: true })
  await url.fill('https://hooks.example.com/new')
  await panel.getByRole('button', { name: '保存', exact: true }).click()
  try {
    await expect.poll(() => attempts).toBe(1)
    await expect(panel.getByRole('button', { name: '保存', exact: true })).toHaveAttribute('aria-busy', 'true')
    for (const input of await panel.locator('input, button').all()) await expect(input).toBeDisabled()
    await page.getByRole('tab', { name: '站点公告', exact: true }).click()
    await page.getByRole('tab', { name: '通知多路', exact: true }).click()
    await expect(url).toHaveValue('https://hooks.example.com/new')
  } finally { release() }
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误' })).toBeVisible()
  await expect(url).toBeEnabled()
  await expect(url).toHaveValue('https://hooks.example.com/new')
  shouldFail = false
  await panel.getByRole('button', { name: '保存', exact: true }).click()
  await expect.poll(() => posts.length).toBe(1)
  await expect(panel.getByRole('button', { name: '保存', exact: true })).toBeEnabled()
  await expect(url).toHaveValue('https://hooks.example.com/new')
  expect(reads()).toBe(1)
  await url.fill('https://hooks.example.com/later')
  await page.getByRole('tab', { name: '站点公告', exact: true }).click()
  await page.getByRole('tab', { name: '通知多路', exact: true }).click()
  await expect(url).toHaveValue('https://hooks.example.com/later')
  await panel.getByRole('button', { name: '取消', exact: true }).click()
  await expect(url).toHaveValue('https://hooks.example.com/new')
})

test('通知配置 320px：收件人、间隔和操作按钮不挤出页面', async ({ page }) => {
  await prepare(page)
  await page.setViewportSize({ width: 320, height: 800 })
  await page.goto('/admin/settings')
  await page.getByRole('tab', { name: '通知多路', exact: true }).click()
  const panel = page.getByRole('tabpanel', { name: '通知多路', exact: true })
  await panel.getByRole('button', { name: '添加邮件', exact: true }).click()
  await panel.locator('#nto-0').fill('oncall-platform-team@example.com, finance@example.com')
  await panel.locator('#nto-0').press('Enter')
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  expect(await panel.evaluate((node) => node.scrollWidth <= node.clientWidth)).toBe(true)
  await panel.locator('#nint-0').fill('')
  await expect(panel.locator('#nint-0')).toHaveValue('')
  await page.locator('#main-content').getByRole('heading', { level: 1 }).scrollIntoViewIfNeeded()
  await page.screenshot({ path: 'test-results/notify-320.png', fullPage: true, animations: 'disabled' })
})

for (const width of [390, 1280]) test(`通知配置 ${width}px：多路分区清晰，中英长内容与操作区不溢出`, async ({ page }) => {
  const { open } = await prepareNotifications(page, [
    { type: 'webhook', url: 'https://hooks.example.com/long-webhook-path-for-platform-alerts', events: ['drift', 'channel_cooldown'], min_interval_secs: 120 },
    { type: 'email', to: ['platform-oncall@example.com', 'finance-operations@example.com'], lang: 'zh-CN', events: ['balance_low', 'margin_breaker'], min_interval_secs: 300 },
  ])
  await page.setViewportSize({ width, height: 900 })
  await open()
  if (width === 390) {
    await page.getByRole('button', { name: '语言', exact: true }).click()
    await page.getByRole('menuitem', { name: 'English', exact: true }).click()
    // 下划线式页签向下重叠 1px；这里检查文字所在按钮的水平范围，不将装饰边线计为遮挡。
    await expect.poll(() => page.getByRole('tab', { name: 'Notifications', exact: true }).evaluate((tab) => {
      const list = tab.closest('[role=tablist]')!.getBoundingClientRect()
      const box = tab.getBoundingClientRect()
      return box.left >= list.left && box.right <= list.right
    })).toBe(true)
    await page.evaluate(() => document.documentElement.classList.add('dark'))
  }
  const panel = page.getByRole('tabpanel').filter({ visible: true })
  await expect(panel.getByRole('region')).toHaveCount(2)
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  expect(await panel.evaluate((node) => node.scrollWidth <= node.clientWidth)).toBe(true)
  if (width === 390) {
    for (const control of await panel.locator('input:not([type=checkbox]), button').all()) {
      if (await control.isVisible()) expect((await control.boundingBox())!.height).toBeGreaterThanOrEqual(44)
    }
  }
  await page.locator('#main-content').getByRole('heading', { level: 1 }).scrollIntoViewIfNeeded()
  await page.screenshot({ path: `test-results/notify-${width}.png`, fullPage: true, animations: 'disabled' })
})

test('设置通知多路：Webhook 与邮件分行提交，事件从清单勾选而不是手拼字符串', async ({ page }) => {
  await prepare(page)
  const posts: Json[] = []
  let stored: unknown[] = []
  await page.route('**/admin/settings/notify_channels', (route) => {
    if (route.request().method() !== 'GET') return route.fallback()
    return route.fulfill({ json: { value: stored } })
  })
  await page.route('**/admin/settings', async (route) => {
    if (route.request().isNavigationRequest()) return route.fallback()
    if (route.request().method() !== 'POST') return route.fallback()
    const body = route.request().postDataJSON() as Json
    stored = body.value as unknown[]
    posts.push(body)
    await route.fulfill({ json: { ok: true } })
  })

  await page.goto('/admin/settings')
  await page.getByRole('tab', { name: '通知多路', exact: true }).click()
  await expect(page.getByText('还没有通知渠道')).toBeVisible()

  await page.getByRole('button', { name: '添加 Webhook', exact: true }).click()
  await page.locator('#nurl-0').fill('https://hooks.example.com/okapi')
  await page.locator('#nint-0').fill('120')
  await page.getByRole('checkbox', { name: '负毛利熔断触发' }).uncheck()

  await page.getByRole('button', { name: '添加邮件', exact: true }).click()
  await page.locator('#nto-1').fill('ops@example.com')
  await page.locator('#nto-1').press('Enter')
  await page.getByRole('button', { name: '中文', exact: true }).click()

  await page.getByRole('button', { name: '保存', exact: true }).click()
  await expect.poll(() => posts.length).toBe(1)
  expect(posts[0]).toEqual({
    key: 'notify_channels',
    value: [
      {
        type: 'webhook',
        url: 'https://hooks.example.com/okapi',
        events: ['drift', 'channel_cooldown', 'balance_low', 'egress_ip_changed', 'egress_down'],
        min_interval_secs: 120,
      },
      {
        type: 'email',
        to: ['ops@example.com'],
        lang: 'zh-CN',
        events: ['drift', 'channel_cooldown', 'balance_low', 'margin_breaker', 'egress_ip_changed', 'egress_down'],
        min_interval_secs: 300,
      },
    ],
  })

  await dismissToasts(page)
  await expect(page.getByRole('button', { name: '删除', exact: true })).toHaveCount(2)
  await page.getByRole('button', { name: '删除', exact: true }).last().click()
  await expect(page.getByRole('button', { name: '删除', exact: true })).toHaveCount(1)
  await page.route('**/admin/settings', (route) => {
    if (route.request().isNavigationRequest() || route.request().method() !== 'POST') return route.fallback()
    return route.fulfill(apiError(500, 'internal_error'))
  })
  await page.getByRole('button', { name: '保存', exact: true }).click()
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误' })).toBeVisible()
  await page.getByRole('alert').getByRole('button', { name: '关闭', exact: true }).click()
  await page.route('**/admin/settings', async (route) => {
    if (route.request().isNavigationRequest() || route.request().method() !== 'POST') return route.fallback()
    const body = route.request().postDataJSON() as Json
    stored = body.value as unknown[]
    posts.push(body)
    await route.fulfill({ json: { ok: true } })
  })
  // 事件清单变长后「保存」落在提示浮层下方；悬停会让提示不消失，先关掉
  await dismissToasts(page)
  await page.getByRole('button', { name: '保存', exact: true }).click()
  await expect.poll(() => posts.length).toBe(2)
  expect(posts[1]).toEqual({
    key: 'notify_channels',
    value: [
      {
        type: 'webhook',
        url: 'https://hooks.example.com/okapi',
        events: ['drift', 'channel_cooldown', 'balance_low', 'egress_ip_changed', 'egress_down'],
        min_interval_secs: 120,
      },
    ],
  })

  await dismissToasts(page)
  await expect(page.getByRole('button', { name: '删除', exact: true })).toHaveCount(1)
  await page.getByRole('button', { name: '删除', exact: true }).click()
  await expect(page.getByText('还没有通知渠道')).toBeVisible()
  await page.route('**/admin/settings', (route) => {
    if (route.request().isNavigationRequest() || route.request().method() !== 'POST') return route.fallback()
    return route.fulfill(apiError(500, 'internal_error'))
  })
  await page.getByRole('button', { name: '保存', exact: true }).click()
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误' })).toBeVisible()
  await page.getByRole('alert').getByRole('button', { name: '关闭', exact: true }).click()
  await page.route('**/admin/settings', async (route) => {
    if (route.request().isNavigationRequest() || route.request().method() !== 'POST') return route.fallback()
    const body = route.request().postDataJSON() as Json
    stored = body.value as unknown[]
    posts.push(body)
    await route.fulfill({ json: { ok: true } })
  })
  await page.getByRole('button', { name: '保存', exact: true }).click()
  await expect.poll(() => posts.length).toBe(3)
  expect(posts[2]).toEqual({ key: 'notify_channels', value: [] })
})

test('公告编辑：加载和读取失败期间不能覆盖默认值，非法配置可重试恢复', async ({ page }) => {
  const state = await prepareNotice(page)
  let release: () => void = () => undefined
  state.readGate = new Promise<void>((resolve) => { release = resolve })
  state.readStatus = 500
  await page.goto('/admin/settings')
  await page.getByRole('tab', { name: '站点公告', exact: true }).click()
  const panel = page.getByRole('tabpanel', { name: '站点公告' })
  try {
    await expect(panel.getByRole('status')).toBeVisible()
    await expect(panel.locator('input, textarea, select')).toHaveCount(0)
    expect(state.posts).toHaveLength(0)
  } finally { release() }
  await expect(panel.getByRole('alert')).toContainText('服务内部错误')
  state.readStatus = 200
  for (const value of [{ body: 12 }, { enabled: 'true' }, { level: ['info'] }]) {
    state.value = value
    await panel.getByRole('button', { name: '重试' }).click()
    await expect(panel.getByRole('alert')).toContainText('公告配置格式不正确')
    await expect(panel.locator('input, textarea, select')).toHaveCount(0)
  }
  state.value = null
  await panel.getByRole('button', { name: '重试' }).click()
  await expect(page.locator('#notice-title')).toHaveValue('')
  await expect(panel.getByRole('switch')).not.toBeChecked()
  await expect(panel.getByRole('button', { name: '保存', exact: true })).toBeDisabled()
  expect(state.posts).toHaveLength(0)
})

test('公告编辑：草稿实时预览与线上已读隔离，切换分区不丢输入，重大通知不能关闭预览', async ({ page }) => {
  const original = { enabled: true, title: '线上标题', body: '线上内容', level: 'warning', updated_at: '2026-09-26T08:00:00Z' }
  const state = await prepareNotice(page, original)
  await page.addInitScript((version) => localStorage.setItem('okapi.notice.dismissed', version), original.updated_at)
  let publicReads = 0
  await page.route('**/api/notice', (route) => { publicReads += 1; return route.fulfill({ json: { notice: original } }) })
  await page.goto('/admin/settings')
  await page.getByRole('tab', { name: '站点公告', exact: true }).click()
  const preview = page.getByRole('region', { name: '公告草稿预览' })
  await expect(preview).toContainText('线上内容')
  await page.locator('#notice-title').fill('  即将维护  ')
  await page.locator('#notice-body').fill('第一行\n<img src=x onerror=alert(1)>')
  await expect(preview).toContainText('即将维护')
  await expect(preview).toContainText('<img src=x onerror=alert(1)>')
  await expect(preview.locator('img')).toHaveCount(0)
  await expect(preview.getByRole('status')).toHaveCount(0)
  await page.getByRole('tab', { name: '注册与风控', exact: true }).click()
  await page.getByRole('tab', { name: '站点公告', exact: true }).click()
  await expect(page.locator('#notice-title')).toHaveValue('  即将维护  ')
  await preview.getByRole('button', { name: '关闭', exact: true }).focus()
  await page.keyboard.press('Enter')
  await expect(preview.getByText('已关闭本次预览')).toBeVisible()
  await expect(preview.getByRole('button', { name: '恢复预览', exact: true })).toBeFocused()
  expect(await page.evaluate(() => localStorage.getItem('okapi.notice.dismissed'))).toBe(original.updated_at)
  await page.keyboard.press('Enter')
  await expect(preview.getByRole('button', { name: '关闭', exact: true })).toBeFocused()
  await expect(preview).toContainText('即将维护')
  await preview.getByRole('button', { name: '关闭', exact: true }).click()
  await page.locator('#notice-title').fill('新的预览')
  await expect(preview).toContainText('新的预览')
  await page.locator('#notice-level').selectOption('critical')
  await expect(preview.getByRole('button', { name: '关闭', exact: true })).toHaveCount(0)
  await expect(preview.getByRole('alert')).toHaveCount(0)
  await expect(preview).toContainText('不会停用服务')
  expect(publicReads).toBe(1)
  expect(state.reads).toBe(1)
  expect(state.posts).toHaveLength(0)
})

test('公告编辑共享横幅：手机无标题正文占满宽度，关闭记录与重大通知行为正确', async ({ page }) => {
  await page.setViewportSize({ width: 320, height: 800 })
  await prepareNotice(page)
  const notice = { title: '', body: '维护安排：https://example.test/' + 'abcdefghij'.repeat(10), level: 'warning', updated_at: 'mobile-warning-v1' }
  await page.route('**/api/notice', (route) => route.fulfill({ json: { notice } }))
  await page.goto('/admin/settings')
  const banner = page.getByRole('status').filter({ hasText: '维护安排：' })
  await expect(banner).toBeVisible()
  expect((await banner.locator('p').boundingBox())!.width).toBeGreaterThan((await banner.boundingBox())!.width * 0.75)
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  await banner.getByRole('button', { name: '关闭', exact: true }).focus()
  await page.keyboard.press('Enter')
  await expect(banner).toHaveCount(0)
  expect(await page.evaluate(() => localStorage.getItem('okapi.notice.dismissed'))).toBe('mobile-warning-v1')
  notice.level = 'critical'
  notice.updated_at = 'mobile-critical-v2'
  await page.reload()
  const critical = page.getByRole('alert').filter({ hasText: '维护安排：' })
  await expect(critical).toBeVisible()
  await expect(critical.getByRole('button', { name: '关闭', exact: true })).toHaveCount(0)
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
})

test('公告编辑：空正文不能发布，emoji 按字符计数，超长输入保留且禁止提交', async ({ page }) => {
  const state = await prepareNotice(page)
  await page.goto('/admin/settings')
  await page.getByRole('tab', { name: '站点公告', exact: true }).click()
  const panel = page.getByRole('tabpanel', { name: '站点公告' })
  await panel.getByRole('switch').check()
  const submit = panel.getByRole('button', { name: '保存并发布', exact: true })
  await expect(submit).toBeDisabled()
  await expect(page.locator('#notice-body')).toHaveAccessibleDescription(/发布前请填写公告正文/)
  await page.locator('#notice-body').fill('  \n  ')
  await expect(submit).toBeDisabled()
  await page.locator('#notice-title').fill('😀'.repeat(80))
  await page.locator('#notice-body').fill('😀'.repeat(4000))
  await expect(page.locator('#notice-title-count')).toHaveText('80 / 80 字')
  await expect(page.locator('#notice-body-count')).toHaveText('4000 / 4000 字')
  await expect(submit).toBeEnabled()
  await page.locator('#notice-title').fill('😀'.repeat(81))
  await page.locator('#notice-body').fill('😀'.repeat(4001))
  await expect(page.locator('#notice-title')).toHaveValue('😀'.repeat(81))
  await expect(page.locator('#notice-body')).toHaveValue('😀'.repeat(4001))
  await expect(page.locator('#notice-title')).toHaveAttribute('aria-invalid', 'true')
  await expect(page.locator('#notice-body')).toHaveAccessibleDescription(/超出的内容已保留/)
  await expect(submit).toBeDisabled()
  expect(state.posts).toHaveLength(0)
  await page.locator('#notice-title').fill('😀'.repeat(80))
  await page.locator('#notice-body').fill('😀'.repeat(4000))
  await submit.click()
  await expect.poll(() => state.posts.length).toBe(1)
  expect(state.posts[0].value).toMatchObject({ title: '😀'.repeat(80), body: '😀'.repeat(4000), enabled: true })
})

test('公告编辑：保存未启用内容、发布和下线操作清楚，取消恢复最近保存版本并保留扩展字段', async ({ page }) => {
  const state = await prepareNotice(page, { enabled: false, title: '未发布标题', body: '旧内容', level: 'info', extra_option: { future: true } })
  await page.goto('/admin/settings')
  await page.getByRole('tab', { name: '站点公告', exact: true }).click()
  const panel = page.getByRole('tabpanel', { name: '站点公告' })
  await page.locator('#notice-title').fill('  保存的标题  ')
  await page.locator('#notice-body').fill('  新内容\n第二行  ')
  await expect(panel).toContainText('有未保存的修改')
  await panel.getByRole('button', { name: '保存', exact: true }).click()
  await expect.poll(() => state.posts.length).toBe(1)
  expect(state.posts[0].value).toMatchObject({ enabled: false, title: '保存的标题', body: '新内容\n第二行', extra_option: { future: true } })
  expect((state.posts[0].value as Json).updated_at).toMatch(/^\d{4}-\d{2}-\d{2}T/)
  await expect(page.locator('#notice-title')).toHaveValue('保存的标题')
  await expect(panel.getByRole('button', { name: '保存', exact: true })).toBeDisabled()
  await dismissToasts(page)
  await panel.getByRole('switch').check()
  await panel.getByRole('button', { name: '保存并发布', exact: true }).click()
  await expect.poll(() => state.posts.length).toBe(2)
  await expect(panel.getByRole('button', { name: '保存并发布', exact: true })).toBeDisabled()
  await dismissToasts(page)
  await panel.getByRole('switch').uncheck()
  await expect(panel.getByRole('button', { name: '保存并下线', exact: true })).toBeEnabled()
  expect(state.posts).toHaveLength(2)
  await panel.getByRole('button', { name: '取消', exact: true }).click()
  await expect(panel.getByRole('switch')).toBeChecked()
  await expect(page.locator('#notice-title')).toHaveValue('保存的标题')
  await panel.getByRole('switch').uncheck()
  await panel.getByRole('button', { name: '保存并下线', exact: true }).click()
  await expect.poll(() => state.posts.length).toBe(3)
  expect(state.posts[2].value).toMatchObject({ enabled: false, title: '保存的标题', body: '新内容\n第二行', extra_option: { future: true } })
  await expect(panel.getByRole('button', { name: '保存', exact: true })).toBeDisabled()
  expect(state.reads).toBe(1)
})

test('公告编辑：保存时锁定表单，失败保留原始草稿，重试成功后取消不退回旧值', async ({ page }) => {
  const state = await prepareNotice(page, { enabled: true, title: '旧标题', body: '旧正文', level: 'warning' })
  await page.goto('/admin/settings')
  await page.getByRole('tab', { name: '站点公告', exact: true }).click()
  const panel = page.getByRole('tabpanel', { name: '站点公告' })
  await page.locator('#notice-title').fill('  新标题  ')
  await page.locator('#notice-body').fill('新正文')
  let release: () => void = () => undefined
  state.saveGate = new Promise<void>((resolve) => { release = resolve })
  state.saveStatus = 500
  await panel.getByRole('button', { name: '保存并发布', exact: true }).click()
  try {
    await expect.poll(() => state.posts.length).toBe(1)
    for (const id of ['notice-title', 'notice-body', 'notice-level']) await expect(page.locator(`#${id}`)).toBeDisabled()
    await expect(panel.getByRole('switch')).toBeDisabled()
    await expect(panel.getByRole('button', { name: '取消', exact: true })).toBeDisabled()
    await expect(panel.getByRole('button', { name: '保存并发布', exact: true })).toHaveAttribute('aria-busy', 'true')
    await page.getByRole('tab', { name: '注册与风控', exact: true }).click()
    await page.getByRole('tab', { name: '站点公告', exact: true }).click()
    await expect(page.locator('#notice-title')).toHaveValue('  新标题  ')
    await expect(page.locator('#notice-title')).toBeDisabled()
  } finally { release() }
  await expect(page.getByRole('alert').filter({ hasText: '服务内部错误' })).toBeVisible()
  await expect(page.locator('#notice-title')).toBeEnabled()
  await expect(page.locator('#notice-title')).toHaveValue('  新标题  ')
  await dismissToasts(page)
  state.saveStatus = 200
  await panel.getByRole('button', { name: '保存并发布', exact: true }).click()
  await expect.poll(() => state.posts.length).toBe(2)
  expect(state.posts[1].value).toMatchObject({ title: '新标题', body: '新正文', enabled: true })
  await expect(page.locator('#notice-title')).toHaveValue('新标题')
  await page.locator('#notice-title').fill('其他修改')
  await panel.getByRole('button', { name: '取消', exact: true }).click()
  await expect(page.locator('#notice-title')).toHaveValue('新标题')
  await expect(panel.getByRole('button', { name: '保存并发布', exact: true })).toBeDisabled()
  expect(state.reads).toBe(1)
})

for (const width of [320, 390, 1280]) {
  test(`公告编辑：${width}px 预览内部滚动，长内容不横向溢出，移动控件可触达`, async ({ page }) => {
    await page.setViewportSize({ width, height: 800 })
    const english = width === 390
    await prepareNotice(page, null, english ? 'en' : 'zh-CN')
    await page.goto('/admin/settings')
    await page.getByRole('tab', { name: english ? 'Site notice' : '站点公告', exact: true }).click()
    const preview = page.getByRole('region', { name: english ? 'Draft preview' : '公告草稿预览' })
    await page.locator('#notice-title').fill(english ? 'Scheduled maintenance' : '服务维护通知')
    await page.locator('#notice-level').selectOption('warning')
    await page.locator('#notice-body').fill('https://example.test/' + 'long-path-'.repeat(160) + '\n维护安排\n'.repeat(25))
    const content = preview.getByRole('group')
    expect(await content.evaluate((node) => node.scrollHeight > node.clientHeight)).toBe(true)
    // 校验实际滚动视口，避免入场 transform 的亚像素误差（320.00003px）误报。
    expect(await content.evaluate((node) => node.clientHeight)).toBeLessThanOrEqual(320)
    expect(await content.evaluate((node) => node.scrollWidth <= node.clientWidth)).toBe(true)
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    if (width < 768) {
      expect((await content.locator('p').boundingBox())!.width).toBeGreaterThan((await content.boundingBox())!.width * 0.75)
      for (const id of ['notice-title', 'notice-level']) expect((await page.locator(`#${id}`).boundingBox())!.height).toBeGreaterThanOrEqual(44)
      expect((await preview.getByRole('button', { name: english ? 'Close' : '关闭', exact: true }).boundingBox())!.height).toBeGreaterThanOrEqual(44)
    } else {
      expect((await preview.boundingBox())!.x).toBeGreaterThan((await page.locator('#notice-title').boundingBox())!.x)
    }
    await page.locator('#notice-body').fill(english ? 'Scheduled maintenance: 02:00–02:30 UTC.\nRequests may be briefly interrupted. Please retry after maintenance.\nWe will update this notice when service is restored.' : '维护时间：02:00–02:30（UTC）。\n期间请求可能短暂中断，建议稍后重试。\n维护完成后，我们会更新此公告。')
    if (english) await page.evaluate(() => document.documentElement.classList.add('dark'))
    await page.evaluate(() => window.scrollTo({ top: 0, left: 0, behavior: 'instant' }))
    await page.screenshot({ path: `test-results/notice-${width}.png`, fullPage: true, animations: 'disabled' })
  })
}
