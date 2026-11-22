import { expect, test } from '@playwright/test'
import type { Page, Route } from '@playwright/test'
import { fileURLToPath } from 'node:url'

type Json = Record<string, unknown>

const channelFixture = {
  id: 42,
  name: 'anthropic-relay',
  provider: 'anthropic',
  api_base: 'https://relay.example.com/v1',
  models: ['claude-sonnet-4-6', 'claude-opus-4-6'],
  model_mapping: {},
  priority: 10,
  weight: 100,
  cost_milli: 1000,
  status: 1,
  pinned: false,
  pools: ['default'],
  keys: [
    { id: 101, status: 1, key_hint: 'sk-ant-…9a', account_label: 'acct-a@example.com', account_plan: null },
    { id: 102, status: 1, key_hint: 'sk-ant-…bc', account_label: 'acct-b@example.com', account_plan: null },
  ],
  settings: {},
  last_test: null,
  last_balance: null,
}

function encodeSse(events: Array<{ event: string; data: unknown }>): string {
  return events.map((e) => `event: ${e.event}\ndata: ${JSON.stringify(e.data)}\n\n`).join('')
}

function verifiedEvents() {
  const bank = {
    loaded: true,
    cache_exists: true,
    built_at: '2026-11-22T00:00:00Z',
    model_count: 75,
    tokenizer_classes: 62,
    tokenizer_probes: 72,
    sources: ['https://lm.ikale.io/data/'],
  }
  const protocol = {
    model_name: {
      declared_model: 'claude-sonnet-4-6',
      upstream_model: 'claude-sonnet-4-6',
      response_model: 'claude-sonnet-4-6',
      declared_family: 'claude',
      response_family: 'claude',
      status: 'pass',
    },
    tool_call: {
      status: 'pass',
      nonce: 'okapi-a1b2c3d4',
      tool_called: true,
      nonce_matched: true,
      arguments_valid_json: true,
      latency_ms: 412,
    },
    signature: {
      status: 'pass',
      applicable: true,
      has_thinking_block: true,
      has_signature: true,
      signature_len: 256,
      thinking_mode: 'adaptive',
      latency_ms: 820,
    },
  }
  const posterior = {
    classes: [
      {
        id: 'openai_o200k',
        lab: 'openai',
        lab_name: 'OpenAI',
        series: 'o200k_base',
        series_zh: 'o200k_base',
        exact: 0.002,
        related: 0.004,
        matched: 0,
        compared: 6,
        overhead: 18,
      },
    ],
    labs: [{ lab: 'openai', lab_name: 'OpenAI', total: 0.006 }],
    unknown: 0.994,
    alien: 0.985,
    answered: 6,
  }
  const tokenizer = {
    posterior,
    verdict: {
      kind: 'unknown',
      top: posterior.classes[0],
      confidence: 0.994,
      claim: {
        expected: [],
        vendor: 'Anthropic',
        first_party: true,
        probability: 0.994,
        status: 'consistent',
      },
    },
    probes_run: 6,
    baseline_tokens: 18,
    baseline_drift: 0,
    latency_ms: 1120,
  }
  const samples = [0, 1, 2].map((index) => ({
    index,
    expected_count: 260,
    parsed_numbers: 260,
    minimum_numbers: 182,
    accepted: true,
    latency_ms: 2100 + index * 300,
    response_model: 'claude-sonnet-4-6',
  }))
  const fingerprint = {
    bank_built_at: '2026-11-22T00:00:00Z',
    bank_model_count: 75,
    decision: 'scored',
    prediction: 'claude-sonnet-4.6',
    prediction_name: 'Claude Sonnet 4.6',
    family_prediction: 'claude',
    family_prediction_name: 'Claude',
    declared_family: 'claude',
    family_matches: true,
    declared_in_bank: true,
    top_probability: 0.89,
    top_family_probability: 0.97,
    probability_status: 'reference_calibrated',
    used_outputs: 3,
    model_like: true,
    evidence_label: 'Strong',
    evidence_reason: 'Top family dominates posterior.',
    top_matches: [
      { model: 'claude-sonnet-4.6', display_name: 'Claude Sonnet 4.6', family: 'claude', family_name: 'Claude', score: 0.92, probability: 0.89 },
      { model: 'claude-opus-4.6', display_name: 'Claude Opus 4.6', family: 'claude', family_name: 'Claude', score: 0.74, probability: 0.08 },
    ],
    family_probabilities: [
      { family: 'claude', family_name: 'Claude', probability: 0.97 },
      { family: 'gpt', family_name: 'GPT', probability: 0.03 },
    ],
    samples,
  }
  const report = {
    channel_id: 42,
    provider: 'anthropic',
    model: 'claude-sonnet-4-6',
    upstream_model: 'claude-sonnet-4-6',
    mimic_profile_active: false,
    verdict: 'verified',
    warnings: [],
    protocol,
    tokenizer,
    fingerprint,
    elapsed_ms: 4820,
  }

  return encodeSse([
    {
      event: 'init',
      data: {
        type: 'init',
        channel_id: 42,
        provider: 'anthropic',
        model: 'claude-sonnet-4-6',
        upstream_model: 'claude-sonnet-4-6',
        mimic_profile_active: false,
        checks: { protocol: true, tokenizer: true, fingerprint: true },
        bank,
      },
    },
    { event: 'stage', data: { type: 'stage', stage: 'protocol', status: 'running' } },
    { event: 'protocol_result', data: { type: 'protocol_result', result: protocol } },
    { event: 'tokenizer_progress', data: { type: 'tokenizer_progress', probes_run: 3, max_probes: 12, posterior } },
    { event: 'tokenizer_result', data: { type: 'tokenizer_result', result: tokenizer } },
    ...samples.map((sample) => ({ event: 'fingerprint_sample', data: { type: 'fingerprint_sample', sample } })),
    { event: 'fingerprint_result', data: { type: 'fingerprint_result', result: fingerprint } },
    { event: 'done', data: { type: 'done', report } },
  ])
}

function mismatchEvents() {
  const posterior = {
    classes: [
      {
        id: 'deepseek_v3',
        lab: 'deepseek',
        lab_name: 'DeepSeek',
        series: 'DeepSeek V3',
        series_zh: 'DeepSeek V3',
        exact: 0.996,
        related: 0.0,
        matched: 6,
        compared: 6,
        overhead: 14,
      },
    ],
    labs: [{ lab: 'deepseek', lab_name: 'DeepSeek', total: 0.996 }],
    unknown: 0.004,
    alien: 0.002,
    answered: 6,
  }
  const tokenizer = {
    posterior,
    verdict: {
      kind: 'exact',
      top: posterior.classes[0],
      confidence: 0.996,
      claim: {
        expected: [],
        vendor: 'Anthropic',
        first_party: true,
        probability: 0.004,
        status: 'inconsistent',
      },
    },
    probes_run: 6,
    baseline_tokens: 14,
    baseline_drift: 0,
    latency_ms: 900,
  }
  const fingerprint = {
    bank_built_at: '2026-11-22T00:00:00Z',
    bank_model_count: 75,
    decision: 'scored',
    prediction: 'deepseek-v3.2',
    prediction_name: 'DeepSeek V3.2',
    family_prediction: 'deepseek',
    family_prediction_name: 'DeepSeek',
    declared_family: 'claude',
    family_matches: false,
    declared_in_bank: true,
    top_probability: 0.94,
    top_family_probability: 0.962,
    probability_status: 'reference_calibrated',
    used_outputs: 3,
    model_like: true,
    evidence_label: 'Strong',
    evidence_reason: 'DeepSeek family dominates.',
    top_matches: [
      { model: 'deepseek-v3.2', display_name: 'DeepSeek V3.2', family: 'deepseek', family_name: 'DeepSeek', score: 0.95, probability: 0.94 },
    ],
    family_probabilities: [
      { family: 'deepseek', family_name: 'DeepSeek', probability: 0.962 },
      { family: 'claude', family_name: 'Claude', probability: 0.012 },
    ],
    samples: [0, 1, 2].map((index) => ({
      index,
      expected_count: 260,
      parsed_numbers: 260,
      minimum_numbers: 182,
      accepted: true,
      latency_ms: 1500,
      response_model: 'claude-opus-4-6',
    })),
  }
  const report = {
    channel_id: 42,
    provider: 'anthropic',
    model: 'claude-opus-4-6',
    upstream_model: 'claude-opus-4-6',
    mimic_profile_active: false,
    verdict: 'mismatch',
    warnings: [
      { code: 'tokenizer_claim_inconsistent', severity: 'error', message: 'Observed DeepSeek V3 tokenizer.' },
      { code: 'fingerprint_family_mismatch', severity: 'error', message: 'Attributed to DeepSeek (96.2%).' },
    ],
    protocol: null,
    tokenizer,
    fingerprint,
    elapsed_ms: 3500,
  }
  return encodeSse([
    { event: 'tokenizer_result', data: { type: 'tokenizer_result', result: tokenizer } },
    { event: 'fingerprint_result', data: { type: 'fingerprint_result', result: fingerprint } },
    { event: 'done', data: { type: 'done', report } },
  ])
}

async function setup(page: Page, sseBody: string) {
  const posts: Array<{ path: string; body: Json | null }> = []
  await page.addInitScript(() => {
    localStorage.setItem('okapi.key', 'verify-fixture-key')
    localStorage.setItem('okapi.lang', 'zh-CN')
  })
  await page.route('**/*', async (route: Route) => {
    const request = route.request()
    const path = new URL(request.url()).pathname
    if (request.isNavigationRequest()) {
      return route.fulfill({
        path: fileURLToPath(new URL('../dist/index.html', import.meta.url)),
        contentType: 'text/html',
      })
    }
    if (!/^\/(api|admin|auth)\//.test(path)) return route.continue()

    if (request.method() === 'POST') {
      posts.push({ path, body: (request.postDataJSON() ?? null) as Json | null })
      if (path === '/admin/channels/verify/bank/refresh') {
        return route.fulfill({
          json: {
            loaded: true,
            cache_exists: true,
            built_at: '2026-11-22T08:00:00Z',
            model_count: 75,
            tokenizer_classes: 62,
            tokenizer_probes: 72,
            sources: ['https://lm.ikale.io/data/'],
          },
        })
      }
      if (path === '/admin/channels/42/verify') {
        return route.fulfill({
          status: 200,
          contentType: 'text/event-stream; charset=utf-8',
          body: sseBody,
        })
      }
      return route.fulfill({ json: {} })
    }

    if (path === '/api/me') {
      return route.fulfill({
        json: { user_id: 1, key_id: 1, group: 'default', balance_micro: 10_000_000, role: 100, permissions: ['*'] },
      })
    }
    if (path === '/admin/channels') {
      return route.fulfill({ json: { data: [channelFixture], total: 1, enabled: 1 } })
    }
    if (path === '/admin/channels/verify/bank') {
      return route.fulfill({
        json: {
          loaded: true,
          cache_exists: true,
          built_at: '2026-11-15T00:00:00Z',
          model_count: 75,
          tokenizer_classes: 62,
          tokenizer_probes: 72,
          sources: ['https://lm.ikale.io/data/'],
        },
      })
    }
    if (path === '/admin/channels/providers') {
      return route.fulfill({
        json: { data: [{ id: 'anthropic', default_base: 'https://api.anthropic.com/v1', account: { quota: false, refresh: false, subscription: null } }] },
      })
    }
    if (path === '/admin/stats/channels' || path === '/admin/stats/channel-usage') {
      return route.fulfill({ json: { data: [], hourly_start: 0 } })
    }
    if (path.startsWith('/admin/settings/')) {
      return route.fulfill({ json: { value: null } })
    }
    return route.fulfill({ json: { data: [] } })
  })
  await page.goto('/admin/channels')
  return posts
}

test('模型验真抽屉：支持刷新参考库并实时流式展示三层校验通过结果', async ({ page }) => {
  const posts = await setup(page, verifiedEvents())

  await page.getByRole('button', { name: '更多操作' }).first().click()
  await page.getByRole('menuitem', { name: '模型验真' }).click()

  const drawer = page.getByRole('dialog')
  await expect(drawer).toBeVisible()
  await expect(drawer.locator('[data-slot="verify-bank-meta"]')).toContainText('指纹库 2026-11-15')
  await expect(drawer.locator('[data-slot="verify-bank-meta"]')).toContainText('分词库 62 类 · 72 探针')

  // 手动刷新 ModelTrace 行为指纹库
  await drawer.getByRole('button', { name: '更新指纹库' }).click()
  await expect(drawer.locator('[data-slot="verify-bank-meta"]')).toContainText('指纹库 2026-11-22')

  // 发起三层流式验真
  await drawer.getByRole('button', { name: '开始验真' }).click()

  const verdict = drawer.locator('[data-slot="verify-verdict"]')
  await expect(verdict).toHaveAttribute('data-verdict', 'verified')
  await expect(verdict).toContainText('校验一致')

  const protoCard = drawer.locator('[data-slot="verify-protocol-card"]')
  await expect(protoCard).toContainText('claude-sonnet-4-6')
  await expect(protoCard).toContainText('okapi-a1b2c3d4')
  await expect(protoCard).toContainText('已验 256 B (adaptive)')

  const tokCard = drawer.locator('[data-slot="verify-tokenizer-card"]')
  await expect(tokCard).toContainText('与声明一致')
  await expect(tokCard).toContainText('99.4%')

  const fpCard = drawer.locator('[data-slot="verify-fingerprint-card"]')
  await expect(fpCard).toContainText('Claude Sonnet 4.6')
  await expect(fpCard).toContainText('89.0%')
  await expect(fpCard).toContainText('97.0%')

  expect(posts.find((p) => p.path === '/admin/channels/42/verify')?.body).toMatchObject({
    model: 'claude-sonnet-4-6',
    checks: { protocol: true, tokenizer: true, fingerprint: true },
  })
})

test('模型验真抽屉：准确标红套壳替换（声明 Claude 但实为 DeepSeek）', async ({ page }) => {
  await setup(page, mismatchEvents())

  await page.getByRole('button', { name: '更多操作' }).first().click()
  await page.getByRole('menuitem', { name: '模型验真' }).click()

  const drawer = page.getByRole('dialog')
  await drawer.getByRole('button', { name: '开始验真' }).click()

  const verdict = drawer.locator('[data-slot="verify-verdict"]')
  await expect(verdict).toHaveAttribute('data-verdict', 'mismatch')
  await expect(verdict).toContainText('疑似套壳')
  await expect(verdict).toContainText('观测到的分词器特征与声明厂商矛盾')
  await expect(verdict).toContainText('行为指纹高置信度归因至其它模型家族')

  await expect(drawer.locator('[data-slot="verify-tokenizer-card"]')).toContainText('与声明矛盾')
  await expect(drawer.locator('[data-slot="verify-tokenizer-card"]')).toContainText('DeepSeek V3')
  await expect(drawer.locator('[data-slot="verify-fingerprint-card"]')).toContainText('DeepSeek V3.2')
})
