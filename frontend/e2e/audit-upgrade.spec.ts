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
          total: 3,
          has_more: false,
          next_before: null,
        },
      })
    }
    return route.fulfill({ json: { data: [] } })
  })

  await page.goto('/admin/audit')
  await expect(page.getByText('共 3 条', { exact: true })).toBeVisible()
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

test('审计日志分页：页码 / 页宽进地址，按 offset 取数，换条件回第一页但保留页宽', async ({ page }) => {
  await page.addInitScript(() => {
    localStorage.setItem('okapi.key', 'audit-paging-fixture')
    localStorage.setItem('okapi.lang', 'zh-CN')
  })
  const TOTAL = 23
  const queries: URL[] = []
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
      return route.fulfill({ json: { user_id: 1, username: 'root', role: 100, permissions: ['*'] } })
    }
    if (path === '/api/site') return route.fulfill({ json: { site_name: 'Okapi' } })
    if (path === '/admin/audit/actions') return route.fulfill({ json: { data: ['channel.update'] } })
    if (path === '/admin/audit') {
      queries.push(url)
      const limit = Number(url.searchParams.get('limit'))
      const offset = Number(url.searchParams.get('offset'))
      const total = url.searchParams.get('target') ? 1 : TOTAL
      // id 倒序：第 n 条（0 起）的 id = TOTAL - n
      const data = Array.from({ length: Math.max(0, Math.min(limit, total - offset)) }, (_, i) => ({
        id: TOTAL - offset - i,
        actor: 'admin:1',
        actor_info: { kind: 'admin', id: 1, label: 'root' },
        action: 'channel.update',
        target: `ch-${TOTAL - offset - i}`,
        detail: { name: `ch-${TOTAL - offset - i}` },
        ip: null,
        user_agent: null,
        trace_id: null,
        actor_key_id: null,
        created_at: '2026-11-22T09:00:00Z',
      }))
      return route.fulfill({ json: { data, total, has_more: offset + data.length < total, next_before: null } })
    }
    return route.fulfill({ json: { data: [] } })
  })

  await page.goto('/admin/audit')
  const main = page.getByRole('main')
  const pager = page.getByRole('navigation', { name: '分页' })
  await expect(page.getByText('共 23 条', { exact: true })).toBeVisible()
  await expect(pager.getByText('1–10 / 共 23')).toBeVisible()
  await expect(main.getByRole('row').filter({ hasText: 'ch-23' })).toBeVisible()
  expect(queries.at(-1)?.searchParams.get('offset')).toBe('0')
  expect(queries.at(-1)?.searchParams.get('limit')).toBe('10')

  // 翻到第 3 页：按 offset=20 取数，地址带 page=3
  await pager.getByRole('button', { name: '3', exact: true }).click()
  await expect(pager.getByText('21–23 / 共 23')).toBeVisible()
  await expect(main.getByRole('row').filter({ hasText: 'ch-3' })).toBeVisible()
  await expect(main.getByRole('row').filter({ hasText: 'ch-23' })).toHaveCount(0)
  expect(queries.at(-1)?.searchParams.get('offset')).toBe('20')
  await expect(page).toHaveURL(/[?&]page=3(&|$)/)

  // 上一页
  await pager.getByRole('button', { name: '上一页' }).click()
  await expect(pager.getByText('11–20 / 共 23')).toBeVisible()
  expect(queries.at(-1)?.searchParams.get('offset')).toBe('10')

  // 换页宽到 20：对齐到新页宽（第 11 条落在第 1 页），地址带 limit=20
  await pager.getByLabel('每页条数').selectOption('20')
  await expect(pager.getByText('1–20 / 共 23')).toBeVisible()
  await expect(page).toHaveURL(/[?&]limit=20(&|$)/)
  expect(queries.at(-1)?.searchParams.get('limit')).toBe('20')

  // 翻到第 2 页再换条件：回第一页，页宽保留
  await pager.getByRole('button', { name: '2', exact: true }).click()
  await expect(pager.getByText('21–23 / 共 23')).toBeVisible()
  await page.locator('#au-target').fill('ch-5')
  await page.getByRole('button', { name: '搜索', exact: true }).click()
  await expect.poll(() => queries.at(-1)?.searchParams.get('target')).toBe('ch-5')
  expect(queries.at(-1)?.searchParams.get('offset')).toBe('0')
  expect(queries.at(-1)?.searchParams.get('limit')).toBe('20')
  await expect(page).not.toHaveURL(/[?&]page=/)
  await expect(page).toHaveURL(/[?&]limit=20(&|$)/)
  await expect(page.getByText('共 1 条', { exact: true })).toBeVisible()
})
