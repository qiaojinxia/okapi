import { expect, test } from '@playwright/test'
import type { Page, Route } from '@playwright/test'
import { fileURLToPath } from 'node:url'
import { diffRules, lineDiff } from '../src/features/compat/diff'
import { freshId, parseRules } from '../src/features/compat/editor'
import { normalizeVendors, presetEndpoints } from '../src/features/channels/compat'

// 适配规则页（docs/compat-rules.md §17）：适配档案、规则从草稿到发布、收件箱审批、蓝图与试跑、历史回滚、
// 参考页、全局护栏，以及渠道抽屉里的厂商预设。接口全部打桩（一个带状态的小型后端），断言提交体的形状。

type Json = Record<string, unknown>
type Rule = Json & { id: string }

interface Row {
  version: number
  status: 'draft' | 'published' | 'archived'
  rules: Rule[]
  note: string | null
  created_by: number | null
  created_at: string
  published_by: number | null
  published_at: string | null
  base_version: number | null
  source: string
  rationale: string | null
}

interface Profile {
  id: number
  slug: string
  name: string
  description: string | null
  extends: string | null
  hosts: string[]
  channels: number
}

const NOW = '2026-10-25T08:00:00Z'
const row = (version: number, status: Row['status'], rules: Rule[], extra: Partial<Row> = {}): Row => ({
  version, status, rules, note: null, created_by: 1, created_at: NOW,
  published_by: status === 'draft' ? null : 1, published_at: status === 'draft' ? null : NOW,
  base_version: null, source: 'admin', rationale: null, ...extra,
})

const STRIP: Rule = { id: 'strip-user', do: [{ op: 'remove', paths: ['/user'] }] }
const DROP_ON_400: Rule = {
  id: 'drop-unsupported-param', phase: 'error',
  when: { status: [400], body_contains: ['unsupported parameter'] },
  do: [{ op: 'retry_same', mutate: [{ op: 'remove', paths: ['/user'] }] }],
}

const CATALOG = {
  schema_version: 1,
  phases: [
    { id: 'request_pre', enabled: false, summary: 'Before dialect conversion.', note: 'reserved', ops: [] },
    { id: 'request', enabled: true, summary: 'The JSON sent upstream.', note: '', ops: ['remove', 'set', 'role_map'] },
    { id: 'wire', enabled: false, summary: 'Headers.', note: 'reserved', ops: [] },
    { id: 'response', enabled: true, summary: 'Upstream response.', note: '', ops: ['remove', 'set'] },
    { id: 'error', enabled: true, summary: 'Upstream error.', note: '', ops: ['retry_same', 'failover', 'fail'] },
    { id: 'capability', enabled: true, summary: 'Facts.', note: '', ops: ['capability'] },
  ],
  ops: [
    { name: 'remove', phases: ['request', 'response'], summary: 'Delete the given paths.', fields: [{ name: 'paths', type: 'pointer[]', required: true, note: '' }] },
    { name: 'set', phases: ['request', 'response'], summary: 'Write a value.', fields: [{ name: 'path', type: 'pointer', required: true, note: '' }, { name: 'value', type: 'json', required: true, note: '' }] },
    { name: 'role_map', phases: ['request'], summary: 'Rename a role.', fields: [{ name: 'from', type: 'string', required: true, note: '' }, { name: 'to', type: 'string', required: true, note: '' }] },
    { name: 'retry_same', phases: ['error'], summary: 'Resend.', fields: [{ name: 'mutate', type: 'ops', required: true, note: '' }] },
    { name: 'brand_new_op', phases: ['request'], summary: 'A new operation the UI has no words for.', fields: [] },
  ],
  selector: {
    fields: [
      { name: 'dialect', type: 'string[]', values: ['openai.chat', 'anthropic.messages'] },
      { name: 'operation', type: 'string[]', values: ['chat', 'embeddings'] },
      { name: 'model', type: 'string[]' },
    ],
    capability_fields: ['vendor', 'host', 'model'], response_dialects: ['openai.chat'],
  },
  when: { fields: [{ name: 'status', type: 'int[]' }, { name: 'body_contains', type: 'string[]' }], semantics: 'all fields' },
  usage_slots: [{ name: 'prompt', pointer: '/usage/prompt_tokens' }, { name: 'completion', pointer: '/usage/completion_tokens' }],
  capability_facts: ['vision', 'tools'],
  protected_paths: {
    request: [{ path: '/model', class: 'protected', node_only: false }, { path: '/messages', class: 'sensitive', node_only: false }],
    response: [{ path: '/usage', class: 'billing_critical', node_only: false }],
  },
  limits: { max_rules: 64, max_retry: 3 },
  recipes: [
    { id: 'strip-fields', title: 'Remove request fields the upstream rejects', rule: { id: 'strip-fields', do: [{ op: 'remove', paths: ['/user', '/logit_bias'] }] } },
    { id: 'developer-role', title: 'Upstream rejects the developer role', rule: { id: 'developer-role', do: [{ op: 'role_map', from: 'developer', to: 'system' }] } },
  ],
}

const VENDORS = {
  data: [
    {
      id: 'builtin:deepseek', display_name: 'DeepSeek', group: 'cn', hosts: ['api.deepseek.com'],
      endpoints: [{ protocol: 'openai.chat', base: 'https://api.deepseek.com/v1' }],
      probes: {
        balance: {
          requests: [{ path: '/user/balance', root: true }],
          remaining: { at: '/balance_infos/0/total_balance' }, currency: 'CNY',
          doc: { why: 'x', verified_at: '2026-10-25' },
        },
      },
      rules: 1,
    },
    {
      id: 'builtin:moonshot', display_name: 'Moonshot / Kimi', group: 'cn', hosts: ['api.moonshot.cn', 'api.moonshot.ai'],
      endpoints: [
        { protocol: 'openai.chat', base: 'https://api.moonshot.cn/v1', label: '国内' },
        { protocol: 'openai.chat', base: 'https://api.moonshot.ai/v1', label: 'International' },
      ],
      probes: {}, rules: 2,
    },
  ],
  profiles: [],
  suggestion: null,
}

/// 带状态的小型后端：规则集 = 每个范围一条已发布 + 至多一条草稿 + 历史。
class Backend {
  profiles: Profile[] = []
  published = new Map<string, Row>()
  drafts = new Map<string, Row>()
  history = new Map<string, Row[]>()
  nextProfile = 100
  calls: { method: string; path: string; body: Json | null }[] = []
  /// 编辑器的实时校验和试跑都走 dry-run，单独记，免得和用例要断言的写请求混在一起。
  dryRuns: Json[] = []

  key = (scope: string, id: number) => `${scope}:${scope === 'guard' ? 0 : id}`

  addProfile(partial: Partial<Profile> & { slug: string; name: string }): Profile {
    const profile: Profile = { id: this.nextProfile++, description: null, extends: null, hosts: [], channels: 0, ...partial }
    this.profiles.push(profile)
    return profile
  }

  publish(scope: string, id: number, rules: Rule[], extra: Partial<Row> = {}): Row {
    const key = this.key(scope, id)
    const versions = this.history.get(key) ?? []
    const live = this.published.get(key)
    if (live) live.status = 'archived'
    const created = row(Math.max(0, ...versions.map((v) => v.version)) + 1, 'published', rules, extra)
    this.published.set(key, created)
    this.history.set(key, [...versions, created])
    return created
  }

  draft(scope: string, id: number, rules: Rule[], extra: Partial<Row> = {}): Row {
    const key = this.key(scope, id)
    const versions = this.history.get(key) ?? []
    const created = row(Math.max(0, ...versions.map((v) => v.version)) + 1, 'draft', rules, {
      base_version: this.published.get(key)?.version ?? 0, ...extra,
    })
    this.drafts.set(key, created)
    this.history.set(key, [...versions.filter((v) => v.status !== 'draft'), created])
    return created
  }

  profileJson(p: Profile) {
    const live = this.published.get(this.key('profile', p.id))
    return {
      ...p, vendor: `custom:${p.slug}`, created_by: 1, created_at: NOW, updated_at: NOW,
      version: live?.version ?? null, rules: live?.rules.length ?? 0, has_draft: this.drafts.has(this.key('profile', p.id)),
    }
  }

  inbox() {
    return [...this.drafts.entries()].map(([key, draft]) => {
      const [scope = '', idText = '0'] = key.split(':')
      const id = Number(idText)
      const live = this.published.get(key)?.version ?? null
      return {
        scope, id, name: scope === 'profile' ? this.profiles.find((p) => p.id === id)?.name : null,
        version: draft.version, base_version: draft.base_version, published_version: live,
        stale: (draft.base_version ?? 0) !== (live ?? 0), source: draft.source, rationale: draft.rationale,
        note: draft.note, created_by: draft.created_by, created_at: draft.created_at, rules: draft.rules.length,
      }
    })
  }

  dryRun(body: Json) {
    const rules = (body.rules ?? []) as Rule[]
    const issues = rules.flatMap((rule) =>
      ((rule.do ?? []) as Json[]).some((op) => op.op === 'remove' && ((op.paths ?? []) as string[]).includes('/model'))
        ? [{ code: 'protected_path', rule: rule.id, index: 0, detail: '/model 是主链字段' }]
        : [],
    )
    if (issues.length > 0) return { valid: false, issues }
    const phase = (body.phase ?? 'request') as string
    const layers = [{ layer: 'channel', label: 'channel#0@v0', rules: rules.length }]
    if (phase === 'capability') return { valid: true, phase, layers, facts: {}, channel_capabilities: null, merged: {} }
    if (phase === 'error') {
      const reactions = rules.filter((r) => r.phase === 'error').map((r) => ({
        rule: r.id, layer: 'channel', action: 'retry_same', viable: true, detail: { max: 1, resend: { model: 'm' } },
      }))
      return { valid: true, phase, layers, matched: reactions.length > 0, chosen: reactions.length > 0 ? 0 : null, reactions }
    }
    const before = (phase === 'response' ? body.response : body.request) as Json
    const after: Json = structuredClone(before)
    const fired: Json[] = []
    for (const rule of rules.filter((r) => (r.phase ?? 'request') === phase)) {
      let changes = 0
      for (const op of (rule.do ?? []) as Json[]) {
        if (op.op !== 'remove') continue
        for (const path of op.paths as string[]) {
          const name = path.slice(1)
          if (name in after) { delete after[name]; changes++ }
        }
      }
      if (changes > 0) fired.push({ rule: rule.id, layer: 'channel', changes })
    }
    return { valid: true, phase, layers, changed: JSON.stringify(after) !== JSON.stringify(before), after, fired, skipped: [] }
  }

  handle(method: string, path: string, body: Json | null): { status?: number; json: unknown } {
    if (path === '/admin/compat/dry-run') this.dryRuns.push(body ?? {})
    else if (method !== 'GET') this.calls.push({ method, path, body })
    const set = path.match(/^\/admin\/compat\/sets\/(channel|profile|guard)\/(\d+)(\/.*)?$/)
    if (set) return this.handleSet(method, set[1] ?? '', Number(set[2]), set[3] ?? '', body)
    if (path === '/admin/compat/catalog') return { json: CATALOG }
    if (path === '/admin/compat/vendors') return { json: VENDORS }
    if (path === '/admin/compat/drafts') return { json: { data: this.inbox() } }
    if (path === '/admin/compat/dry-run') return { json: this.dryRun(body ?? {}) }
    if (path === '/admin/compat/profiles' && method === 'GET') return { json: { data: this.profiles.map((p) => this.profileJson(p)) } }
    if (path === '/admin/compat/profiles' && method === 'POST') {
      const request = body ?? {}
      if (this.profiles.some((p) => p.slug === request.slug)) return { status: 409, json: { error: { code: 'profile_slug_taken' } } }
      return { json: this.profileJson(this.addProfile({
        slug: String(request.slug), name: String(request.name),
        description: (request.description as string | undefined) ?? null,
        extends: request.extends ? String(request.extends) : null, hosts: (request.hosts as string[]) ?? [],
      })) }
    }
    const profile = path.match(/^\/admin\/compat\/profiles\/(\d+)$/)
    if (profile) {
      const found = this.profiles.find((p) => p.id === Number(profile[1]))
      if (!found) return { status: 404, json: { error: { code: 'not_found' } } }
      if (method === 'GET') {
        const key = this.key('profile', found.id)
        return { json: {
          ...this.profileJson(found), published: this.published.get(key) ?? null, draft: this.drafts.get(key) ?? null,
          effective: { layers: [], rules: [], shadowed: [] },
        } }
      }
      if (method === 'PATCH') {
        Object.assign(found, body ?? {})
        return { json: this.profileJson(found) }
      }
      if (method === 'DELETE') {
        if (found.channels > 0) return { status: 409, json: { error: { code: 'profile_in_use' } } }
        this.profiles = this.profiles.filter((p) => p.id !== found.id)
        return { json: { ok: true } }
      }
    }
    return { status: 404, json: { error: { code: 'not_found' } } }
  }

  handleSet(method: string, scope: string, id: number, tail: string, body: Json | null): { status?: number; json: unknown } {
    const key = this.key(scope, id)
    if (tail === '' && method === 'GET') {
      const live = this.published.get(key) ?? null
      const draft = this.drafts.get(key) ?? null
      return { json: { scope, id, published: live, draft, stale: draft !== null && (draft.base_version ?? 0) !== (live?.version ?? 0) } }
    }
    if (tail === '/versions') {
      return { json: { data: (this.history.get(key) ?? []).map((v) => ({ ...v, rules: v.rules.length })).reverse() } }
    }
    const version = tail.match(/^\/versions\/(\d+)$/)
    if (version) {
      const found = (this.history.get(key) ?? []).find((v) => v.version === Number(version[1]))
      return found ? { json: found } : { status: 404, json: { error: { code: 'not_found' } } }
    }
    if (tail === '/draft' && method === 'PUT') {
      const live = this.published.get(key)?.version ?? 0
      if ((body?.base_version as number | undefined) !== live) return { status: 409, json: { error: { code: 'rules_version_conflict' } } }
      return { json: { ok: true, version: this.draft(scope, id, body?.rules as Rule[], { note: (body?.note as string | undefined) ?? null }).version } }
    }
    if (tail === '/draft' && method === 'DELETE') {
      this.drafts.delete(key)
      this.history.set(key, (this.history.get(key) ?? []).filter((v) => v.status !== 'draft'))
      return { json: { ok: true } }
    }
    if (tail === '/draft/publish') {
      const draft = this.drafts.get(key)
      if (!draft) return { status: 404, json: { error: { code: 'not_found' } } }
      this.drafts.delete(key)
      const live = this.published.get(key)
      if (live) live.status = 'archived'
      draft.status = 'published'
      draft.published_at = NOW
      this.published.set(key, draft)
      return { json: { ok: true, version: draft.version } }
    }
    if (tail === '/rollback') {
      const old = (this.history.get(key) ?? []).find((v) => v.version === body?.version)
      return old ? { json: { ok: true, version: this.publish(scope, id, old.rules).version } } : { status: 404, json: { error: { code: 'not_found' } } }
    }
    if (tail === '/rules' && method === 'PUT') {
      return { json: { ok: true, version: this.publish(scope, id, body?.rules as Rule[], { note: (body?.note as string | undefined) ?? null }).version } }
    }
    return { status: 404, json: { error: { code: 'not_found' } } }
  }
}

async function prepare(page: Page, backend: Backend, { permissions = ['*'] } = {}) {
  await page.addInitScript(() => {
    localStorage.setItem('okapi.key', 'compat-page-fixture')
    localStorage.setItem('okapi.lang', 'zh-CN')
  })
  await page.route('**/*', async (route: Route) => {
    const request = route.request()
    const url = new URL(request.url())
    if (request.isNavigationRequest()) {
      return route.fulfill({ path: fileURLToPath(new URL('../dist/index.html', import.meta.url)), contentType: 'text/html' })
    }
    if (!/^\/(api|admin|auth)\//.test(url.pathname)) return route.continue()
    if (url.pathname === '/api/me') {
      return route.fulfill({ json: { user_id: 1, key_id: 1, group: 'default', balance_micro: 0, balance_expires_at: null, role: 100, permissions } })
    }
    if (url.pathname === '/api/notice') return route.fulfill({ json: { notice: null } })
    if (url.pathname.startsWith('/admin/compat/')) {
      const raw = request.postData()
      const out = backend.handle(request.method(), url.pathname, raw ? (JSON.parse(raw) as Json) : null)
      return route.fulfill({ status: out.status ?? 200, json: out.json })
    }
    if (url.pathname === '/admin/channels/providers') {
      return route.fulfill({ json: { data: [
        { id: 'openai', default_base: 'https://api.openai.com/v1', account: { quota: false, refresh: false, authorization: null, subscription: null } },
        { id: 'openai_compat', default_base: null, account: { quota: false, refresh: false, authorization: null, subscription: null } },
      ] } })
    }
    expect(request.method(), `未被桩接住的写请求：${url.pathname}`).toBe('GET')
    return route.fulfill({ json: url.pathname.startsWith('/admin/settings/') ? { value: null } : { data: [], total: 0, next_before: null } })
  })
}

const dialog = (page: Page) => page.getByRole('dialog')

// ----------------------------------------------------------------------------- 纯逻辑

test('规则差异：按 id 对齐，改动逐行比对，顺序变化单独标出', () => {
  const a: Rule = { id: 'a', do: [{ op: 'remove', paths: ['/x'] }] }
  const b: Rule = { id: 'b', do: [{ op: 'remove', paths: ['/y'] }] }
  const a2: Rule = { id: 'a', do: [{ op: 'remove', paths: ['/x', '/z'] }] }
  const c: Rule = { id: 'c', do: [] }
  const diff = diffRules([a, b], [a2, c])
  expect(diff.changes.map((x) => [x.id, x.kind])).toEqual([['a', 'changed'], ['c', 'added'], ['b', 'removed']])
  expect(diff.differs).toBe(true)
  expect(diffRules([a, b], [a, b]).differs).toBe(false)
  const swapped = diffRules([a, b], [b, a])
  expect(swapped.reordered).toBe(true)
  expect(swapped.changes.every((x) => x.kind === 'unchanged')).toBe(true)
  expect(diffRules([], []).differs).toBe(false)
  // 逐行：只多了一行
  const lines = lineDiff('1\n2\n3', '1\n2\n2b\n3')
  expect(lines.filter((l) => l.kind === 'add').map((l) => l.text)).toEqual(['2b'])
  expect(lines.filter((l) => l.kind === 'del')).toHaveLength(0)
  expect(lineDiff('x', 'x').every((l) => l.kind === 'same')).toBe(true)
  // 超大输入不做 O(n·m) 比对，整段当作删了又加
  const big = Array.from({ length: 450 }, (_, i) => String(i)).join('\n')
  expect(lineDiff(big, big + '\nz').some((l) => l.kind === 'same')).toBe(false)
})

test('编辑器辅助：只认 JSON 数组，配方 id 冲突自动加后缀，预设端点从厂商或档案叠加的厂商取', () => {
  expect(parseRules('[{"id":"a"}]')).toEqual([{ id: 'a' }])
  for (const bad of ['', '{', '{"id":"a"}', '"x"', 'null']) expect(parseRules(bad), bad).toBeNull()
  expect(freshId(['a'], 'b')).toBe('b')
  expect(freshId(['a', 'a-2'], 'a')).toBe('a-3')
  const vendors = normalizeVendors({
    data: [{ id: 'builtin:deepseek', display_name: 'DeepSeek', group: 'cn', hosts: [], rules: 1, probes: {},
      endpoints: [{ protocol: 'openai.chat', base: 'https://api.deepseek.com/v1' }, { protocol: 'anthropic.messages', base: 'https://api.deepseek.com/anthropic' }] }],
    profiles: [{ id: 'custom:acme', slug: 'acme', display_name: 'Acme', description: null, extends: 'builtin:deepseek', hosts: [] },
      { id: 'custom:bare', slug: 'bare', display_name: 'Bare', description: null, extends: null, hosts: [] }],
  })
  expect(presetEndpoints(vendors, 'builtin:deepseek').map((e) => e.base)).toEqual(['https://api.deepseek.com/v1'])
  expect(presetEndpoints(vendors, 'custom:acme').map((e) => e.base)).toEqual(['https://api.deepseek.com/v1'])
  expect(presetEndpoints(vendors, 'custom:bare')).toEqual([])
  expect(presetEndpoints(vendors, 'builtin:nope')).toEqual([])
  expect(presetEndpoints(vendors, null)).toEqual([])
  expect(presetEndpoints(undefined, 'builtin:deepseek')).toEqual([])
  expect(presetEndpoints(vendors, 'builtin:deepseek', 'anthropic.messages')).toHaveLength(1)
  // 缺字段按没有：旧版本接口 / 桩数据不该让下拉崩
  expect(normalizeVendors({ data: [{ id: 'x', display_name: 'X' }] as never })).toMatchObject({ profiles: [], data: [{ hosts: [], endpoints: [], probes: {} }] })
  expect(normalizeVendors(null)).toEqual({ data: [], profiles: [] })
})

// ----------------------------------------------------------------------------- 页面

test('档案：新建（标识格式先在前端拦）→ 规则存为草稿 → 批准发布 → 历史里是线上版本', async ({ page }) => {
  const backend = new Backend()
  backend.addProfile({ slug: 'acme', name: 'Acme Relay', extends: 'builtin:deepseek', hosts: ['relay.example.com'] })
  await prepare(page, backend)
  await page.goto('/admin/compat')

  await expect(page.getByRole('heading', { name: '适配规则' })).toBeVisible()
  const table = page.getByRole('table', { name: '适配档案' })
  await expect(table.getByText('Acme Relay')).toBeVisible()
  await expect(table.getByText('custom:acme')).toBeVisible()
  await expect(table.getByText('DeepSeek')).toBeVisible()
  await expect(table.getByText('未发布')).toBeVisible()

  await page.getByRole('button', { name: '新建适配档案' }).first().click()
  const create = dialog(page)
  await expect(create.locator(':focus')).toHaveCount(1)
  await create.getByLabel('标识（slug）').fill('Bad Slug')
  await create.getByLabel('名称').fill('Nova')
  await create.getByRole('button', { name: '新建适配档案' }).click()
  await expect(create.getByText('标识要以小写字母或数字开头')).toBeVisible()
  expect(backend.calls).toEqual([])

  await create.getByLabel('标识（slug）').fill('nova')
  await create.getByLabel('叠加的内置厂商').selectOption({ label: 'Moonshot / Kimi' })
  await create.getByRole('button', { name: '新建适配档案' }).click()
  await expect(dialog(page).getByRole('heading', { name: '档案：Nova' })).toBeVisible()
  expect(backend.calls[0]).toEqual({
    method: 'POST', path: '/admin/compat/profiles',
    body: { slug: 'nova', name: 'Nova', extends: 'builtin:moonshot', hosts: [] },
  })

  // 新档案没有规则：从配方起手，校验通过后存为草稿（有发布权限时另有「保存并发布」）
  const drawer = dialog(page)
  await expect(drawer.getByText('还没有发布过规则')).toBeVisible()
  await drawer.getByLabel('插入配方…').selectOption({ label: '删掉上游不认的请求字段' })
  await expect(drawer.getByLabel('规则（JSON）')).toHaveValue(/"id": "strip-fields"/)
  await expect(drawer.getByText('校验通过 · 1 条规则')).toBeVisible()
  await expect(drawer.getByRole('button', { name: '保存并发布' })).toBeEnabled()
  await drawer.getByLabel('备注（可选）').fill('first cut')
  await drawer.getByRole('button', { name: '存为草稿' }).click()
  await expect(drawer.getByText('有一份待批准的草稿（v1）')).toBeVisible()
  expect(backend.calls[1]).toEqual({
    method: 'PUT', path: '/admin/compat/sets/profile/101/draft',
    body: { rules: [CATALOG.recipes[0]?.rule], base_version: 0, note: 'first cut' },
  })

  // 编辑器与草稿一致时才能批准；批准后横幅消失，线上版本出现
  await expect(drawer.getByRole('button', { name: '批准并发布' })).toBeEnabled()
  await drawer.getByRole('button', { name: '批准并发布' }).click()
  await expect(drawer.getByText('v1 · 1 条规则')).toBeVisible()
  await expect(drawer.getByText('有一份待批准的草稿')).toHaveCount(0)
  expect(backend.calls[2]).toEqual({ method: 'POST', path: '/admin/compat/sets/profile/101/draft/publish', body: null })

  await drawer.getByRole('tab', { name: '历史' }).click()
  const history = drawer.getByRole('table', { name: '版本历史' })
  await expect(history.getByRole('row', { name: /v1.*线上/ })).toBeVisible()
})

test('没有发布权限：只能提交审批，看不到待批准页签；全局护栏没有写权限就只读', async ({ page }) => {
  const backend = new Backend()
  backend.addProfile({ slug: 'acme', name: 'Acme Relay' })
  await prepare(page, backend, { permissions: ['channel.read', 'compat.profile.write'] })
  await page.goto('/admin/compat')
  await expect(page.getByRole('tab', { name: /适配档案/ })).toBeVisible()
  await expect(page.getByRole('tab', { name: /待批准/ })).toHaveCount(0)

  await page.getByRole('button', { name: '编辑规则' }).click()
  const drawer = dialog(page)
  await drawer.getByLabel('规则（JSON）').fill('[{"id":"r","do":[{"op":"remove","paths":["/user"]}]}]')
  await expect(drawer.getByText('校验通过 · 1 条规则')).toBeVisible()
  await expect(drawer.getByRole('button', { name: '保存并发布' })).toHaveCount(0)
  await drawer.getByRole('button', { name: '提交审批' }).click()
  await expect(drawer.getByText('有一份待批准的草稿（v1）')).toBeVisible()
  // 草稿不是线上版本，也批准不了（没有 compat.publish）
  await expect(drawer.getByRole('button', { name: '批准并发布' })).toHaveCount(0)
  await page.keyboard.press('Escape')

  await page.getByRole('tab', { name: '全局护栏' }).click()
  await expect(page.getByLabel('规则（JSON）')).toHaveAttribute('readonly', '')
  await expect(page.getByText('你没有修改这份规则的权限，只能查看。')).toBeVisible()
  await expect(page.getByRole('button', { name: '提交审批' })).toHaveCount(0)
})

test('收件箱：AI 的提议带来源与依据，审阅时看到差异，过期的草稿批准不了', async ({ page }) => {
  const backend = new Backend()
  const acme = backend.addProfile({ slug: 'acme', name: 'Acme Relay' })
  const stale = backend.addProfile({ slug: 'old', name: 'Old Relay' })
  backend.publish('profile', acme.id, [STRIP])
  backend.draft('profile', acme.id, [{ ...STRIP, do: [{ op: 'remove', paths: ['/user', '/logit_bias'] }] }, DROP_ON_400], {
    source: 'mcp', rationale: '上游返回 400 unsupported parameter: user',
  })
  backend.publish('profile', stale.id, [STRIP])
  backend.draft('profile', stale.id, [STRIP, DROP_ON_400], { source: 'mcp', rationale: '过期的提议' })
  backend.publish('profile', stale.id, [STRIP, DROP_ON_400]) // 草稿起草后线上版本又变了
  await prepare(page, backend)
  await page.goto('/admin/compat')

  await page.getByRole('tab', { name: /待批准/ }).click()
  const inbox = page.getByRole('table', { name: '待批准' })
  await expect(inbox.getByRole('row')).toHaveCount(3)
  const fresh = inbox.getByRole('row', { name: /Acme Relay/ })
  await expect(fresh.getByText('AI 提议')).toBeVisible()
  await expect(fresh.getByText('上游返回 400 unsupported parameter: user')).toBeVisible()
  await expect(inbox.getByRole('row', { name: /Old Relay/ }).getByText('已过期')).toBeVisible()

  await fresh.getByRole('button', { name: '审阅' }).click()
  const drawer = dialog(page)
  await expect(drawer.getByText('依据：上游返回 400 unsupported parameter: user')).toBeVisible()
  await drawer.getByRole('button', { name: '查看差异' }).click()
  await expect(drawer.getByText('1 条新增 · 1 条修改 · 0 条删除')).toBeVisible()
  await expect(drawer.locator('pre').getByText('"/logit_bias"')).toBeVisible()
  await drawer.getByRole('button', { name: '批准并发布' }).click()
  await expect(drawer.getByText('有一份待批准的草稿')).toHaveCount(0)
  expect(backend.calls.at(-1)).toMatchObject({ method: 'POST', path: `/admin/compat/sets/profile/${acme.id}/draft/publish` })
  await page.keyboard.press('Escape')

  // 过期的那份：横幅提示，批准按钮禁用
  await page.getByRole('row', { name: /Old Relay/ }).getByRole('button', { name: '审阅' }).click()
  await expect(dialog(page).getByText('已过期：起草后线上版本又变了')).toBeVisible()
  await expect(dialog(page).getByRole('button', { name: '批准并发布' })).toBeDisabled()
  // 丢弃它
  await dialog(page).getByRole('button', { name: '丢弃草稿' }).click()
  await page.getByRole('alertdialog', { name: '丢弃这份草稿？' }).getByRole('button', { name: '丢弃草稿' }).click()
  await expect(dialog(page).getByText('有一份待批准的草稿')).toHaveCount(0)
  expect(backend.calls.at(-1)).toMatchObject({ method: 'DELETE', path: `/admin/compat/sets/profile/${stale.id}/draft` })
})

test('编辑器：JSON 不合法与引擎拒绝的规则都挡住保存，并指出哪条规则什么问题', async ({ page }) => {
  const backend = new Backend()
  const acme = backend.addProfile({ slug: 'acme', name: 'Acme Relay' })
  backend.publish('profile', acme.id, [STRIP])
  await prepare(page, backend)
  await page.goto('/admin/compat')
  await page.getByRole('button', { name: '编辑规则' }).click()
  const drawer = dialog(page)
  const editor = drawer.getByLabel('规则（JSON）')
  await expect(editor).toHaveValue(/"strip-user"/)
  await expect(drawer.getByText('校验通过 · 1 条规则')).toBeVisible()
  await expect(drawer.getByRole('button', { name: '存为草稿' })).toBeDisabled()

  await editor.fill('[{"id": ')
  await expect(drawer.getByText('不是合法的 JSON 数组。')).toBeVisible()
  await expect(drawer.getByRole('button', { name: '存为草稿' })).toBeDisabled()
  await expect(drawer.getByRole('button', { name: '格式化' })).toBeDisabled()
  await drawer.getByLabel('插入配方…').selectOption({ label: '上游拒收 developer 角色' })
  await expect(page.getByRole('alert').filter({ hasText: '编辑器里的 JSON 不合法' })).toBeVisible()

  await editor.fill('[{"id":"bad","do":[{"op":"remove","paths":["/model"]}]}]')
  const problems = drawer.getByRole('alert').filter({ hasText: '有 1 个问题' })
  await expect(problems).toBeVisible()
  await expect(problems.getByText('bad')).toBeVisible()
  await expect(problems.getByText('protected_path')).toBeVisible()
  await expect(drawer.getByRole('button', { name: '存为草稿' })).toBeDisabled()
  await expect(drawer.getByRole('button', { name: '保存并发布' })).toBeDisabled()

  // 还原回基线；改好之后能看到未保存的差异
  await drawer.getByRole('button', { name: '还原' }).click()
  await expect(editor).toHaveValue(/"strip-user"/)
  await editor.fill('[{"id":"strip-user","do":[{"op":"remove","paths":["/user","/seed"]}]}]')
  await expect(drawer.getByText('校验通过 · 1 条规则')).toBeVisible()
  await drawer.getByRole('button', { name: '查看未保存的改动' }).click()
  await expect(drawer.getByText('0 条新增 · 1 条修改 · 0 条删除')).toBeVisible()
})

test('蓝图：规则画成选择→条件→操作的流程，新操作也有节点；试跑后打上命中标记，试跑页签给前后差异', async ({ page }) => {
  const backend = new Backend()
  const acme = backend.addProfile({ slug: 'acme', name: 'Acme Relay', extends: 'builtin:deepseek' })
  backend.publish('profile', acme.id, [
    { ...STRIP, select: { model: ['gpt-*'] } },
    DROP_ON_400,
    { id: 'fancy', do: [{ op: 'brand_new_op', knob: 3 }] },
  ])
  await prepare(page, backend)
  await page.goto('/admin/compat')
  await page.getByRole('button', { name: '编辑规则' }).click()
  const drawer = dialog(page)
  await drawer.getByRole('tab', { name: '蓝图' }).click()

  const strip = drawer.getByRole('listitem').filter({ has: page.getByRole('button', { name: 'strip-user' }) })
  await expect(strip.getByText('model: gpt-*')).toBeVisible()
  await expect(strip.getByText('1. 删除字段')).toBeVisible()
  await expect(strip.getByText('paths: /user')).toBeVisible()
  const error = drawer.getByRole('listitem').filter({ has: page.getByRole('button', { name: 'drop-unsupported-param' }) })
  await expect(error.getByText('status: 400')).toBeVisible()
  await expect(error.getByText('1. 原地重发')).toBeVisible()
  // 目录里有、语言包里还没有名字的操作：照样出节点，用原名
  const fancy = drawer.getByRole('listitem').filter({ has: page.getByRole('button', { name: 'fancy' }) })
  await expect(fancy.getByText('1. brand_new_op')).toBeVisible()
  await expect(fancy.getByText('所有请求')).toBeVisible()
  // 选中一条规则，右侧是它的 JSON
  await error.getByRole('button', { name: 'drop-unsupported-param' }).click()
  await expect(drawer.getByRole('complementary', { name: '规则内容（JSON）' })).toContainText('"retry_same"')

  // 试跑请求阶段：样本里有 user 字段，被 strip-user 删掉
  await drawer.getByRole('tab', { name: '试跑' }).click()
  await expect(drawer.getByRole('button', { name: '试跑' })).toBeEnabled()
  await drawer.getByLabel('请求体（发往上游的 JSON）').fill('{"model":"gpt-4o","user":"u-1"}')
  await drawer.getByRole('button', { name: '试跑', exact: true }).click()
  const result = drawer.getByRole('region', { name: '试跑结果' })
  await expect(result.getByText('有改动')).toBeVisible()
  await expect(result.getByText('strip-user')).toBeVisible()
  await expect(result.getByText('1 处')).toBeVisible()
  expect(backend.dryRuns.at(-1)).toMatchObject({
    vendor: 'builtin:deepseek', phase: 'request', context: { model: 'gpt-4o', dialect: 'openai.chat', operation: 'chat', stream: false },
  })

  // 回到蓝图：被改动的那条规则有标记
  await drawer.getByRole('tab', { name: '蓝图' }).click()
  await expect(strip.getByText('改了 1 处')).toBeVisible()
  await expect(error.getByText('改了')).toHaveCount(0)

  // 错误阶段：按顺序给出会采用的反应
  await drawer.getByRole('tab', { name: '试跑' }).click()
  await drawer.getByRole('button', { name: '错误', exact: true }).click()
  await drawer.getByRole('button', { name: '试跑', exact: true }).click()
  await expect(result.getByText('原地重发')).toBeVisible()
  await expect(result.getByText('采用', { exact: true })).toBeVisible()
  // 样本不合法就不让跑，并说明原因
  await drawer.getByRole('button', { name: '请求', exact: true }).click()
  await drawer.getByLabel('请求体（发往上游的 JSON）').fill('{oops')
  await expect(drawer.getByText('请求样本不是合法的 JSON')).toBeVisible()
  await expect(drawer.getByRole('button', { name: '试跑', exact: true })).toBeDisabled()
})

test('历史：旧版本相对线上版本的差异，回滚要确认并把旧内容作为新版本发布', async ({ page }) => {
  const backend = new Backend()
  const acme = backend.addProfile({ slug: 'acme', name: 'Acme Relay' })
  backend.publish('profile', acme.id, [STRIP])
  backend.publish('profile', acme.id, [STRIP, DROP_ON_400], { note: 'add retry' })
  await prepare(page, backend)
  await page.goto('/admin/compat')
  await page.getByRole('button', { name: '编辑规则' }).click()
  const drawer = dialog(page)
  await drawer.getByRole('tab', { name: '历史' }).click()
  const table = drawer.getByRole('table', { name: '版本历史' })
  await expect(table.getByRole('row', { name: /v2.*线上.*add retry/ })).toBeVisible()
  const old = table.getByRole('row', { name: /v1.*历史/ })
  await expect(old.getByRole('button', { name: '回滚到此版' })).toBeVisible()
  await expect(table.getByRole('row', { name: /v2/ }).getByRole('button', { name: '回滚到此版' })).toHaveCount(0)

  await old.getByRole('button', { name: '查看' }).click()
  await expect(drawer.getByText('相对线上版本 v2 的差异')).toBeVisible()
  await expect(drawer.getByText('0 条新增 · 0 条修改 · 1 条删除')).toBeVisible()

  await old.getByRole('button', { name: '回滚到此版' }).click()
  await page.getByRole('alertdialog', { name: '回滚到 v1？' }).getByRole('button', { name: '回滚到此版' }).click()
  await expect(table.getByRole('row', { name: /v3.*线上/ })).toBeVisible()
  expect(backend.calls.at(-1)).toEqual({ method: 'POST', path: `/admin/compat/sets/profile/${acme.id}/rollback`, body: { version: 1 } })
})

test('档案设置：改说明与主机；被渠道占用的档案不能删，空档案要输入标识确认', async ({ page }) => {
  const backend = new Backend()
  backend.addProfile({ slug: 'busy', name: 'Busy', channels: 2 })
  backend.addProfile({ slug: 'idle', name: 'Idle' })
  await prepare(page, backend)
  await page.goto('/admin/compat')

  await page.getByRole('row', { name: /Busy/ }).getByRole('button', { name: '编辑规则' }).click()
  let drawer = dialog(page)
  await drawer.getByRole('tab', { name: '设置' }).click()
  await expect(drawer.getByText('2 个渠道在用', { exact: true })).toBeVisible()
  await expect(drawer.getByRole('button', { name: '删除档案' })).toBeDisabled()
  await expect(drawer.getByText('还有 2 个渠道在用，先把它们的厂商改掉才能删除。')).toBeVisible()
  await drawer.getByLabel('说明').fill('中转商 A')
  await drawer.getByLabel('主机建议').fill('relay.example.com')
  await drawer.getByLabel('主机建议').press('Enter')
  await drawer.getByRole('button', { name: '保存', exact: true }).click()
  await expect.poll(() => backend.calls.at(-1)?.method).toBe('PATCH')
  expect(backend.calls.at(-1)?.body).toEqual({
    name: 'Busy', description: '中转商 A', extends: null, hosts: ['relay.example.com'],
  })
  await page.keyboard.press('Escape')

  await page.getByRole('row', { name: /Idle/ }).getByRole('button', { name: '编辑规则' }).click()
  drawer = dialog(page)
  await drawer.getByRole('tab', { name: '设置' }).click()
  await drawer.getByRole('button', { name: '删除档案' }).click()
  const confirm = page.getByRole('alertdialog', { name: '删除档案「Idle」？' })
  await confirm.getByRole('textbox').fill('idle')
  await confirm.getByRole('button', { name: '删除', exact: true }).click()
  await expect(page.getByRole('row', { name: /Idle/ })).toHaveCount(0)
  expect(backend.calls.at(-1)).toMatchObject({ method: 'DELETE' })
})

test('参考页：内置厂商的接入点与声明式探针、操作表读目录（新操作自动多一行）、配方可复制', async ({ page }) => {
  const backend = new Backend()
  await prepare(page, backend)
  await page.goto('/admin/compat')
  await page.getByRole('tab', { name: '参考' }).click()

  const vendors = page.getByRole('table', { name: '内置厂商' })
  const deepseek = vendors.getByRole('row', { name: /DeepSeek/ })
  await expect(deepseek.getByText('https://api.deepseek.com/v1')).toBeVisible()
  await expect(deepseek.getByText('GET （站点根） /user/balance')).toBeVisible()
  await expect(deepseek.getByText('剩余 ← /balance_infos/0/total_balance')).toBeVisible()
  await expect(deepseek.getByText('2026-10-25 核对')).toBeVisible()
  const moonshot = vendors.getByRole('row', { name: /Moonshot/ })
  await expect(moonshot.getByText('国内')).toBeVisible()
  await expect(moonshot.getByText('通用口径')).toBeVisible()

  await page.getByText('规则能做的 5 种操作').click()
  const ops = page.getByRole('table', { name: '操作' })
  await expect(ops.getByRole('row', { name: /删除字段/ })).toBeVisible()
  await expect(ops.getByRole('row', { name: /brand_new_op/ })).toBeVisible()

  await page.getByText('2 条现成写法').click()
  await expect(page.getByText('删掉上游不认的请求字段')).toBeVisible()
  await expect(page.getByRole('button', { name: '复制配方' })).toHaveCount(2)
})

// ----------------------------------------------------------------------------- 渠道抽屉

test('新建兼容渠道：厂商预设带上接入地址；多接入点可选；手填的地址不被预设覆盖；提交带 vendor', async ({ page }) => {
  const backend = new Backend()
  backend.addProfile({ slug: 'acme', name: 'Acme Relay', extends: 'builtin:moonshot' })
  VENDORS.profiles = [{ id: 'custom:acme', slug: 'acme', display_name: 'Acme Relay', description: null, extends: 'builtin:moonshot', hosts: [] }] as never
  const posted: Json[] = []
  await prepare(page, backend)
  await page.route('**/admin/channels', async (route) => {
    if (route.request().method() !== 'POST') return route.fallback()
    posted.push(route.request().postDataJSON() as Json)
    return route.fulfill({ json: { channel_id: 9, channel_key_id: 1 } })
  })
  await page.goto('/admin/channels')
  await page.getByRole('button', { name: '新建渠道' }).first().click()
  const drawer = dialog(page)
  await expect(drawer.locator(':focus')).toHaveCount(1)
  await drawer.locator('#d-name').fill('relay-1')
  await expect(drawer.getByLabel('厂商预设')).toHaveCount(0)
  await drawer.locator('#d-provider').selectOption('openai_compat')

  const preset = drawer.getByLabel('厂商预设')
  const base = drawer.locator('#d-base')
  await expect(base).toHaveValue('')
  await preset.selectOption({ label: 'DeepSeek' })
  await expect(base).toHaveValue('https://api.deepseek.com/v1')
  // 换预设：地址还是上一个预设填的，就跟着换；Moonshot 有两个接入点，可以再选
  await preset.selectOption({ label: 'Moonshot / Kimi' })
  await expect(base).toHaveValue('https://api.moonshot.cn/v1')
  await drawer.getByLabel('接入点').selectOption({ label: 'International · https://api.moonshot.ai/v1' })
  await expect(base).toHaveValue('https://api.moonshot.ai/v1')
  // 手填的地址不是任何预设的端点：再换预设也不动它
  await base.fill('https://my-gateway.example.com/v1')
  await preset.selectOption({ label: 'DeepSeek' })
  await expect(base).toHaveValue('https://my-gateway.example.com/v1')
  // 适配档案也能当预设，地址取它叠加的厂商的第一个接入点
  await base.fill('')
  await preset.selectOption({ label: 'Acme Relay（适配档案）' })
  await expect(base).toHaveValue('https://api.moonshot.cn/v1')

  await drawer.locator('#d-cred').fill('sk-relay')
  await drawer.locator('#d-models').fill('kimi-k2')
  await drawer.locator('#d-models').press('Enter')
  await drawer.getByRole('button', { name: '新建', exact: true }).click()
  await expect.poll(() => posted.length).toBe(1)
  expect(posted[0]).toMatchObject({
    name: 'relay-1', provider: 'openai_compat', api_base: 'https://api.moonshot.cn/v1', vendor: 'custom:acme',
  })
})

test('新建渠道换协议：预设不再适用，它填的地址一并清掉，请求体不带 vendor', async ({ page }) => {
  const backend = new Backend()
  const posted: Json[] = []
  await prepare(page, backend)
  await page.route('**/admin/channels', async (route) => {
    if (route.request().method() !== 'POST') return route.fallback()
    posted.push(route.request().postDataJSON() as Json)
    return route.fulfill({ json: { channel_id: 9, channel_key_id: 1 } })
  })
  await page.goto('/admin/channels')
  await page.getByRole('button', { name: '新建渠道' }).first().click()
  const drawer = dialog(page)
  await expect(drawer.locator(':focus')).toHaveCount(1)
  await drawer.locator('#d-provider').selectOption('openai_compat')
  await drawer.getByLabel('厂商预设').selectOption({ label: 'DeepSeek' })
  await expect(drawer.locator('#d-base')).toHaveValue('https://api.deepseek.com/v1')
  await drawer.locator('#d-provider').selectOption('openai')
  await expect(drawer.getByLabel('厂商预设')).toHaveCount(0)
  await drawer.locator('#d-name').fill('plain')
  await drawer.locator('#d-cred').fill('sk-1')
  await drawer.locator('#d-models').fill('gpt-5')
  await drawer.locator('#d-models').press('Enter')
  await drawer.getByRole('button', { name: '新建', exact: true }).click()
  await expect.poll(() => posted.length).toBe(1)
  expect(posted[0]?.api_base).toBe('')
  expect(posted[0]).not.toHaveProperty('vendor')
})
