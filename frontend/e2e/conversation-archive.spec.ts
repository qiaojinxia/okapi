import { expect, test } from '@playwright/test'
import type { Page, Route } from '@playwright/test'
import { fileURLToPath } from 'node:url'

type Json = Record<string, unknown>

const ARCHIVE_ID = '11111111-2222-4333-8444-555555555555'

const archiveRecord = {
  request_id: ARCHIVE_ID,
  user_id: 7,
  api_key_id: 12,
  channel_id: 3,
  model: 'claude-sonnet-4-6',
  requested_model: 'claude-sonnet-4-6',
  endpoint: '/v1/messages',
  is_stream: true,
  status_code: 200,
  error_code: '',
  request_body: JSON.stringify({
    model: 'claude-sonnet-4-6',
    system: 'You are concise.',
    messages: [{ role: 'user', content: '21 乘以 19 等于多少？' }],
  }),
  response_body: JSON.stringify({
    stream: true,
    text: '21 × 19 = 399',
    reasoning: '20^2 - 1 = 399',
    tool_calls: [{ id: 'tool_1', name: 'calc', arguments: '{"expr":"21*19"}' }],
  }),
  response_text: '21 × 19 = 399',
  reasoning_text: '20^2 - 1 = 399',
  tool_calls: JSON.stringify([{ id: 'tool_1', name: 'calc', arguments: '{"expr":"21*19"}' }]),
  truncated: true,
  created_at: '2026-11-22T08:00:00Z',
  storage: 'postgres',
}

async function setup(page: Page, options: { clickhouseEnabled?: boolean } = {}) {
  const calls: Array<{ method: string; path: string; body: Json | null }> = []
  let savedPolicy = {
    enabled: false,
    retention_days: 30,
    allow_user_view: false,
    max_body_kb: 256,
    cleanup_interval_hours: 6,
  }
  const chEnabled = options.clickhouseEnabled ?? false

  await page.addInitScript(() => {
    localStorage.setItem('okapi.key', 'archive-fixture-key')
    localStorage.setItem('okapi.lang', 'zh-CN')
  })

  await page.route('**/*', async (route: Route) => {
    const req = route.request()
    const url = new URL(req.url())
    const path = url.pathname
    if (req.isNavigationRequest()) {
      return route.fulfill({
        path: fileURLToPath(new URL('../dist/index.html', import.meta.url)),
        contentType: 'text/html',
      })
    }
    if (!/^\/(api|admin|auth)\//.test(path)) return route.continue()

    if (req.method() !== 'GET') {
      const body = (req.postDataJSON() ?? null) as Json | null
      calls.push({ method: req.method(), path, body })
      if (path === '/admin/settings' && body?.key === 'conversation_archive') {
        savedPolicy = body.value as typeof savedPolicy
        return route.fulfill({ json: { ok: true, requires_publish: false } })
      }
      if (path === '/admin/conversations/cleanup') {
        return route.fulfill({
          json: {
            ok: true,
            retention_days: savedPolicy.retention_days,
            pg_deleted: 14,
            ch_deleted_requested: chEnabled,
            cutoff: '2026-08-24T00:00:00Z',
          },
        })
      }
      if (req.method() === 'DELETE' && path === `/admin/conversations/${ARCHIVE_ID}`) {
        return route.fulfill({ json: { ok: true, deleted: true } })
      }
      return route.fulfill({ json: { ok: true } })
    }

    if (path === '/api/me') {
      return route.fulfill({
        json: {
          user_id: 1,
          key_id: 1,
          group: 'default',
          balance_micro: 10_000_000,
          role: 100,
          permissions: ['*'],
        },
      })
    }
    if (path === '/admin/settings/record_ip_log') {
      return route.fulfill({ json: { value: true } })
    }
    if (path === '/admin/settings/conversation_archive') {
      return route.fulfill({ json: { value: savedPolicy } })
    }
    if (path === '/admin/conversations/stats') {
      return route.fulfill({
        json: {
          backend: chEnabled ? 'clickhouse' : 'postgres',
          clickhouse_enabled: chEnabled,
          total_count: 128,
          pg_count: chEnabled ? 0 : 128,
          ch_count: chEnabled ? 128 : null,
          oldest_at: '2026-10-01T00:00:00Z',
          newest_at: '2026-11-22T08:00:00Z',
          policy: savedPolicy,
        },
      })
    }
    if (path === '/admin/conversations') {
      return route.fulfill({
        json: {
          backend: chEnabled ? 'clickhouse' : 'postgres',
          clickhouse_enabled: chEnabled,
          data: [
            {
              request_id: ARCHIVE_ID,
              user_id: 7,
              username: 'alice',
              api_key_id: 12,
              channel_id: 3,
              channel_name: 'anthropic-main',
              model: 'claude-sonnet-4-6',
              requested_model: 'claude-sonnet-4-6',
              endpoint: '/v1/messages',
              is_stream: true,
              status_code: 200,
              error_code: '',
              request_preview: '21 乘以 19 等于多少？',
              response_preview: '21 × 19 = 399',
              truncated: true,
              created_at: '2026-11-22T08:00:00Z',
              storage: chEnabled ? 'clickhouse' : 'postgres',
            },
          ],
        },
      })
    }
    if (path === `/admin/logs/${ARCHIVE_ID}/conversation`) {
      return route.fulfill({
        json: { data: { ...archiveRecord, storage: chEnabled ? 'clickhouse' : 'postgres' } },
      })
    }
    if (path === '/admin/logs' || path === '/admin/logs/stat') {
      return route.fulfill({
        status: 501,
        json: { error: { code: 'stats_disabled' } },
      })
    }
    if (path.startsWith('/admin/settings/')) {
      return route.fulfill({ json: { value: null } })
    }
    return route.fulfill({ json: { data: [] } })
  })

  return calls
}

test('隐私与留痕：支持切换对话存档开关、永久/定时清理、用户可见性与立即清理（PG 自动降级标识）', async ({ page }) => {
  const calls = await setup(page, { clickhouseEnabled: false })
  await page.goto('/admin/settings?tab=privacy')

  const card = page.locator('[data-slot="conversation-archive-card"]')
  await expect(card).toBeVisible()
  await expect(card.locator('[data-slot="archive-backend-badge"]')).toContainText('PostgreSQL')
  await expect(card.locator('[data-slot="archive-stats-bar"]')).toContainText('128')

  // 开启存档与用户可见
  await card.getByRole('switch', { name: /启用对话内容存档/ }).click()
  await card.getByRole('switch', { name: /允许用户在门户查看自己的对话存档/ }).click()

  // 先保存为永久保留
  await card.getByRole('button', { name: '永久保留' }).click()
  await card.getByRole('button', { name: '保存' }).click()
  await expect.poll(() => calls.find((c) => c.path === '/admin/settings')?.body).toMatchObject({
    key: 'conversation_archive',
    value: {
      enabled: true,
      allow_user_view: true,
      retention_days: 0,
      max_body_kb: 256,
      cleanup_interval_hours: 6,
    },
  })
  // 永久保留时立即清理按钮禁用
  await expect(card.getByRole('button', { name: '立即清理过期存档' })).toBeDisabled()

  // 切回定时清理 90 天并保存，随后触发手动立即清理
  await card.getByRole('button', { name: '定时清理' }).click()
  await card.getByRole('button', { name: '90 天' }).click()
  await card.getByRole('button', { name: '保存' }).click()
  await expect.poll(() =>
    calls.filter((c) => c.path === '/admin/settings').at(-1)?.body,
  ).toMatchObject({
    key: 'conversation_archive',
    value: { retention_days: 90 },
  })

  await card.getByRole('button', { name: '立即清理过期存档' }).click()
  const confirmDialog = page.getByRole('alertdialog')
  await expect(confirmDialog).toContainText('90 天')
  await confirmDialog.getByRole('button', { name: '立即清理过期存档' }).click()
  await expect.poll(() => calls.some((c) => c.path === '/admin/conversations/cleanup')).toBe(true)
})

test('全站日志：纯 PG 模式下可切到「对话存档」页签查看对话气泡、思考过程、工具调用、原始 JSON 并删除', async ({ page }) => {
  const calls = await setup(page, { clickhouseEnabled: false })
  await page.goto('/admin/logs')

  await page.getByRole('button', { name: '对话存档' }).click()
  const tab = page.locator('[data-slot="admin-conversations-tab"]')
  await expect(tab).toBeVisible()
  await expect(tab).toContainText('PostgreSQL')
  await expect(tab).toContainText('claude-sonnet-4-6')
  await expect(tab).toContainText('21 × 19 = 399')

  await tab.getByText('21 × 19 = 399').click()
  const drawer = page.getByRole('dialog')
  await expect(drawer).toBeVisible()
  const viewer = drawer.locator('[data-slot="conversation-viewer"]')
  await expect(viewer).toContainText('You are concise.')
  await expect(viewer).toContainText('21 乘以 19 等于多少？')
  await expect(viewer).toContainText('21 × 19 = 399')
  await expect(viewer).toContainText('已截断')
  await expect(viewer).toContainText('calc')

  // 展开思考过程
  await viewer.getByText('思考过程（点击展开）').click()
  await expect(viewer).toContainText('20^2 - 1 = 399')

  // 切换原始 JSON 视图
  await viewer.getByRole('button', { name: '原始 JSON' }).click()
  await expect(viewer).toContainText('"endpoint"').catch(async () => {
    await expect(viewer).toContainText('"claude-sonnet-4-6"')
  })

  // 删除该条存档
  await drawer.getByRole('button', { name: '删除' }).click()
  await page.getByRole('alertdialog').getByRole('button', { name: '删除' }).click()
  await expect.poll(() =>
    calls.some((c) => c.method === 'DELETE' && c.path === `/admin/conversations/${ARCHIVE_ID}`),
  ).toBe(true)
})
