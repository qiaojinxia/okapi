import { expect, test } from '@playwright/test'
import { fileURLToPath } from 'node:url'

test('审计增强：上下文展示、变更对比、删除快照、Worker 徽标、多维筛选与导出', async ({ page }) => {
  await page.addInitScript(() => {
    localStorage.setItem('okapi.key', 'audit-upgrade-fixture')
    localStorage.setItem('okapi.lang', 'zh-CN')
  })

  const queries: URL[] = []
  const exports: URL[] = []

  await page.route('**/*', (route) => {
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

    if (path === '/api/me') {
      return route.fulfill({
        json: { user_id: 1, username: 'root', role: 100, permissions: ['*'] },
      })
    }
    if (path === '/api/notices') return route.fulfill({ json: { data: [] } })
    if (path === '/api/site') return route.fulfill({ json: { site_name: 'Okapi' } })
    if (path === '/admin/audit/actions') {
      return route.fulfill({
        json: {
          data: [
            'channel.update',
            'channel.delete',
            'margin_breaker.trip',
            'auth.permission_denied',
          ],
        },
      })
    }
    if (path === '/admin/audit/export') {
      exports.push(url)
      const fmt = url.searchParams.get('format')
      if (fmt === 'json') {
        return route.fulfill({
          status: 200,
          contentType: 'application/json; charset=utf-8',
          headers: {
            'x-audit-export-count': '3',
            'x-audit-export-truncated': 'false',
          },
          body: JSON.stringify({ count: 3, truncated: false, data: [] }),
        })
      }
      return route.fulfill({
        status: 200,
        contentType: 'text/csv; charset=utf-8',
        headers: {
          'x-audit-export-count': '5000',
          'x-audit-export-truncated': 'true',
        },
        body: '\uFEFFid,created_at,actor\n1,2026-11-22T00:00:00Z,admin:1\n',
      })
    }
    if (path === '/admin/audit') {
      queries.push(url)
      return route.fulfill({
        json: {
          data: [
            {
              id: 103,
              actor: 'admin:1',
              actor_info: { kind: 'admin', id: 1, label: 'root' },
              action: 'channel.update',
              target: '42',
              detail: {
                before: { priority: 0, name: 'old-ch' },
                after: { priority: 10, name: 'new-ch' },
              },
              ip: '198.51.100.77',
              user_agent: 'OkapiBrowser/2.0',
              trace_id: '4bf92f3577b34da6a3ce929d0e0e4736',
              actor_key_id: 99,
              created_at: '2026-11-22T10:00:00Z',
            },
            {
              id: 102,
              actor: 'admin:1',
              actor_info: { kind: 'admin', id: 1, label: 'root' },
              action: 'channel.delete',
              target: '41',
              detail: {
                deleted: { name: 'legacy-openai', provider: 'openai', models_count: 5 },
              },
              ip: '198.51.100.77',
              user_agent: 'OkapiBrowser/2.0',
              trace_id: 'aaaa2f3577b34da6a3ce929d0e0e4736',
              actor_key_id: 99,
              created_at: '2026-11-22T09:50:00Z',
            },
            {
              id: 101,
              actor: 'worker',
              actor_info: { kind: 'worker', id: null, label: null },
              action: 'margin_breaker.trip',
              target: 'vip:42',
              detail: { group_code: 'vip', channel_id: 42, margin_bp: -250 },
              ip: null,
              user_agent: null,
              trace_id: null,
              actor_key_id: null,
              created_at: '2026-11-22T09:40:00Z',
            },
          ],
          has_more: false,
          next_before: null,
        },
      })
    }
    return route.fulfill({ json: { data: [] } })
  })

  await page.goto('/admin/audit')
  await expect(page.getByText('已加载 3 条')).toBeVisible()
  // Worker 徽标与变更摘要
  await expect(page.getByText('后台任务', { exact: false })).toBeVisible()
  await expect(page.getByText('变更：priority, name')).toBeVisible()
  await expect(page.getByText('198.51.100.77').first()).toBeVisible()

  // 点 channel.update 行：校验请求上下文与 Before -> After 变更对比
  await page.getByRole('row').filter({ hasText: 'channel.update' }).click()
  const detail = page.getByRole('dialog', { name: '审计详情' })
  await expect(detail).toBeVisible()
  await expect(detail.getByText('请求上下文')).toBeVisible()
  await expect(detail.getByText('99', { exact: true })).toBeVisible()
  await expect(detail.getByText('4bf92f3577b34da6a3ce929d0e0e4736', { exact: true })).toBeVisible()
  await expect(detail.getByText('OkapiBrowser/2.0', { exact: true })).toBeVisible()
  await expect(detail.getByText('变更对比（Before → After）')).toBeVisible()
  await expect(detail.getByText('old-ch')).toBeVisible()
  await expect(detail.getByText('new-ch')).toBeVisible()
  await detail.getByRole('button', { name: '关闭', exact: true }).click()

  // 点 channel.delete 行：校验删除前快照
  await page.getByRole('row').filter({ hasText: 'channel.delete' }).click()
  await expect(detail.getByText('删除前快照')).toBeVisible()
  await expect(detail.getByText(/legacy-openai/)).toBeVisible()
  await detail.getByRole('button', { name: '关闭', exact: true }).click()

  // IP / 关键词 / Trace ID 筛选进 URL 与后端请求参数
  await page.locator('#au-ip').fill('198.51.100.0/24')
  await page.locator('#au-keyword').fill('legacy-openai')
  await page.locator('#au-trace').fill('4bf92f3577b34da6a3ce929d0e0e4736')
  await page.getByRole('button', { name: '搜索', exact: true }).click()
  await expect.poll(() => {
    const last = queries.at(-1)
    return (
      last?.searchParams.get('ip') === '198.51.100.0/24' &&
      last?.searchParams.get('q') === 'legacy-openai' &&
      last?.searchParams.get('trace_id') === '4bf92f3577b34da6a3ce929d0e0e4736'
    )
  }).toBe(true)

  // CSV 导出（带截断提示）与 JSON 导出
  await page.getByRole('button', { name: '导出 CSV' }).click()
  await expect.poll(() => exports.some((u) => u.searchParams.get('format') === 'csv')).toBe(true)
  await expect(page.getByText(/达到单次 5000 条上限/)).toBeVisible()

  await page.getByRole('button', { name: '导出 JSON' }).click()
  await expect.poll(() => exports.some((u) => u.searchParams.get('format') === 'json')).toBe(true)
})
