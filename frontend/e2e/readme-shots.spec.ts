import { expect, test } from '@playwright/test'
import type { Page, Route } from '@playwright/test'
import { fileURLToPath } from 'node:url'

// README 截图：真实前端构建 + 接口桩里的虚构演示数据（渠道、账号、请求都是编的，账号用 example.com），不连任何后端。
// 用法（frontend/ 下）：pnpm build && pnpm exec playwright test -c playwright.readme.config.ts
// 输出 docs/screenshots/<name>.png（1440×900 视口，2 倍像素，Asia/Shanghai，简体中文）；README_SHOTS_DIR 可改输出目录。

type Json = Record<string, unknown>
const OUT = process.env.README_SHOTS_DIR ? `${process.env.README_SHOTS_DIR.replace(/\/$/, '')}/` : fileURLToPath(new URL('../../docs/screenshots/', import.meta.url))
const NOW = new Date('2026-11-20T07:30:00Z') // 15:30 Asia/Shanghai
const ago = (min: number) => new Date(NOW.getTime() - min * 60_000).toISOString()
const nowSec = Math.floor(NOW.getTime() / 1000)
const DAYS = ['2026-11-14', '2026-11-15', '2026-11-16', '2026-11-17', '2026-11-18', '2026-11-19', '2026-11-20']
// 一周的起伏：周末低、工作日高（请求数）
const LOAD = [6120, 4980, 8840, 10260, 11730, 12410, 9380]
const MODELS = ['claude-sonnet-4-5', 'gpt-5.1', 'gemini-2.5-pro', 'deepseek-chat', 'qwen3-coder']
const MODEL_SHARE = [0.38, 0.27, 0.16, 0.12, 0.07]
// 没桩到的接口回空列表；跑完列出来，界面加了新接口时据此补桩
const unknown: Set<string> = new Set()

const sum = (xs: number[]) => xs.reduce((a, b) => a + b, 0)

function trendRow(day: string, i: number) {
  const requests = LOAD[i]
  const prompt = requests * 3150
  const completion = requests * 690
  const cached = Math.round(prompt * 0.46)
  const amount = requests * 6420
  return {
    bucket: day, requests, errors: Math.round(requests * (0.006 + (i % 3) * 0.002)), error_rate_bp: 60 + (i % 3) * 20,
    prompt_tokens: prompt, completion_tokens: completion, cached_tokens: cached, cache_write_tokens: Math.round(prompt * 0.04),
    reasoning_tokens: Math.round(completion * 0.3), tokens: prompt + completion, cache_hit_bp: 4600,
    amount_micro: amount, discount_micro: Math.round(amount * 0.06), upstream_cost_micro: Math.round(amount * 0.71),
    avg_latency_ms: 2100 + (i % 4) * 140, avg_ttft_ms: 640 + (i % 3) * 45, ttft_samples: requests, avg_output_tps_milli: 61_000 + i * 900,
    active_users: 40 + i * 3,
  }
}
const TREND = DAYS.map(trendRow)
const total = (rows = TREND) => {
  const t = (k: keyof (typeof TREND)[number]) => sum(rows.map((r) => Number(r[k])))
  return {
    requests: t('requests'), errors: t('errors'), error_rate_bp: 82, amount_micro: t('amount_micro'), discount_micro: t('discount_micro'),
    upstream_cost_micro: t('upstream_cost_micro'), tokens: t('tokens'), prompt_tokens: t('prompt_tokens'), completion_tokens: t('completion_tokens'),
    cached_tokens: t('cached_tokens'), cache_write_tokens: t('cache_write_tokens'), reasoning_tokens: t('reasoning_tokens'), cache_hit_bp: 4600,
    cache_read_known_requests: t('requests'), cache_write_known_requests: t('requests'),
    avg_latency_ms: 2240, avg_ttft_ms: 680, ttft_samples: t('requests'), avg_output_tps_milli: 63_500,
    cost_known_requests: t('requests'), cost_coverage_bp: 10000, known_cost_micro: t('upstream_cost_micro'),
    known_margin_micro: t('amount_micro') - t('upstream_cost_micro'), margin_micro: t('amount_micro') - t('upstream_cost_micro'), margin_rate_bp: 2900,
  }
}
const calendar = (start = DAYS[0], end = DAYS[6]) => ({ start_date: start, end_date: end, today: DAYS[6], timezone: 'Asia/Shanghai', generated_at: '2026-11-20 15:30:00' })

// ---- channels
const key = (over: Json = {}) => ({ id: 1, status: 1, failed_count: 0, cooldown_until: null, last_error: null, weight: 1, max_concurrency: null, credential_kind: 0, ...over })
const base = { priority: 0, settings: {}, pools: ['default'], pool_members: [], cost_milli: 1000, egress: { mode: 'inherit' }, last_test: null, last_balance: null }
const CHANNELS: Json[] = [
  { ...base, id: 1, name: 'anthropic-official', provider: 'anthropic', api_base: 'https://api.anthropic.com/v1', status: 1, priority: 10,
    models: ['claude-sonnet-4-5', 'claude-haiku-4-5', 'claude-opus-4-1'], keys: [key({ id: 11 }), key({ id: 12 })],
    last_test: { ok: true, latency_ms: 812, at: ago(18) } },
  { ...base, id: 2, name: 'openai-primary', provider: 'openai', api_base: 'https://api.openai.com/v1', status: 1, priority: 10,
    models: ['gpt-5.1', 'gpt-5-mini', 'o4-mini', 'text-embedding-3-large'], keys: [key({ id: 21 }), key({ id: 22 }), key({ id: 23 })],
    egress: { mode: 'direct' }, last_test: { ok: true, latency_ms: 604, at: ago(10) },
    last_balance: { probe: 'openai', currency: 'USD', balance_micro: 482_100_000, at: ago(30) } },
  { ...base, id: 3, name: 'vertex-gemini', provider: 'vertex', api_base: 'https://us-central1-aiplatform.googleapis.com', status: 1, priority: 8,
    models: ['gemini-2.5-pro', 'gemini-2.5-flash'], keys: [key({ id: 31 })], last_test: { ok: true, latency_ms: 955, at: ago(32) } },
  { ...base, id: 4, name: 'claude-max · ops@example.com', provider: 'anthropic_max', api_base: 'https://api.anthropic.com', status: 1, priority: 6,
    models: ['claude-sonnet-4-5', 'claude-opus-4-1'],
    keys: [key({ id: 41, credential_kind: 1, oauth_refreshable: true, credential_expires_at: nowSec + 5 * 3600, account_label: 'ops@example.com', account_plan: 'max_5x' })],
    egress: { mode: 'proxy', proxy_id: 1 }, last_test: { ok: true, latency_ms: 1180, at: ago(25) } },
  { ...base, id: 5, name: 'azure-eastus', provider: 'azure', api_base: 'https://okapi-demo.openai.azure.com', status: 1, priority: 5,
    models: ['gpt-5.1', 'gpt-5-mini'], keys: [key({ id: 51 })], last_test: { ok: true, latency_ms: 731, at: ago(28) } },
  { ...base, id: 6, name: 'bedrock-us-west-2', provider: 'bedrock', api_base: 'https://bedrock-runtime.us-west-2.amazonaws.com', status: 1, priority: 4,
    models: ['claude-sonnet-4-5'], keys: [key({ id: 61 })], last_test: { ok: true, latency_ms: 1043, at: ago(50) } },
  { ...base, id: 7, name: 'deepseek-official', provider: 'openai_compat', api_base: 'https://api.deepseek.com/v1', status: 1, priority: 3,
    models: ['deepseek-chat', 'deepseek-reasoner'], keys: [key({ id: 71 }), key({ id: 72, status: 2, cooldown_until: new Date(NOW.getTime() + 18 * 60_000).toISOString(), last_error: 'rate_limited' })],
    last_test: { ok: true, latency_ms: 388, at: ago(15) },
    last_balance: { probe: 'deepseek', currency: 'CNY', balance_micro: 236_450_000, at: ago(30) } },
  { ...base, id: 8, name: 'qwen-dashscope', provider: 'openai_compat', api_base: 'https://dashscope.aliyuncs.com/compatible-mode/v1', status: 1, priority: 3,
    models: ['qwen3-coder', 'qwen3-max'], keys: [key({ id: 81 })], last_test: { ok: false, latency_ms: 0, error_code: 'upstream_timeout', at: ago(140) } },
  { ...base, id: 9, name: 'backup-relay', provider: 'openai_compat', api_base: 'https://relay.example.com/v1', status: 2, priority: 0,
    models: ['gpt-5-mini'], keys: [key({ id: 91 })] },
]
const PROVIDERS = {
  data: [
    ...['openai', 'openai_compat', 'anthropic', 'vertex', 'azure', 'bedrock', 'gemini'].map((id) => ({ id, account: { quota: false, refresh: false, subscription: null } })),
    { id: 'anthropic_max', account: { quota: true, refresh: true, subscription: { quota_scope: 'session', window_secs: 18000, quota_windows: [18000, 604800] } } },
    { id: 'codex', account: { quota: true, refresh: true, subscription: { quota_scope: 'total', window_secs: null, quota_windows: [18000, 604800] } } },
  ],
}
const CH_REQ = [21840, 15620, 9310, 4120, 3260, 2980, 4890, 2240, 0]
// 渠道列表「近 24h」成功率条：48 个半小时槽；qwen 那条（下标 7）下午几个槽出错，停用的那条没流量
const SLOT = 1800
const HOURLY_START = Math.floor(nowSec / SLOT) * SLOT - 47 * SLOT
const hourly = (i: number) => Array.from({ length: 48 }, (_, s) => {
  const requests = i === 8 ? 0 : Math.max(1, Math.round((CH_REQ[i] / 7 / 48) * (0.55 + 0.45 * Math.sin(((s + 14) / 48) * Math.PI))))
  const errors = i === 7 && s >= 38 && s <= 43 ? Math.round(requests * (s === 40 || s === 41 ? 0.4 : 0.08)) : i === 1 && s === 20 ? 1 : 0
  return { ts: HOURLY_START + s * SLOT, requests, errors }
})
const CHANNEL_HEALTH = { hourly_start: HOURLY_START, data: CHANNELS.map((c, i) => ({ hourly: hourly(i),
  channel_id: c.id, name: c.name, provider: c.provider, requests: Math.round(CH_REQ[i] / 7), errors: i === 7 ? 41 : Math.round(CH_REQ[i] / 7 / 300),
  error_rate_bp: i === 7 ? 1830 : 30 + i * 7, ttft_p50_ms: 420 + i * 60, ttft_p95_ms: 1100 + i * 140, ttft_p99_ms: 1900 + i * 200,
  failovers: i === 7 ? 12 : i, sticky_rate_bp: 6200, tokens_per_1k_sec: 0, amount_micro: CH_REQ[i] * 900 })) }
const CHANNEL_USAGE = { data: CHANNELS.map((c, i) => ({
  channel_id: c.id, requests: CH_REQ[i], prompt_tokens: CH_REQ[i] * 3100, cached_tokens: CH_REQ[i] * 1400, completion_tokens: CH_REQ[i] * 680,
  amount_micro: CH_REQ[i] * 6400, upstream_cost_micro: CH_REQ[i] * 4500 })) }
const QUOTAS: Record<string, Json> = {
  '4': { timezone: 'UTC', quotas: [{ key_id: 41, quota: { observed_at: nowSec - 120, allowed: true, windows: [
    { name: '5h', used_percent: 42, resets_at: nowSec + 2 * 3600 + 15 * 60, window_secs: 18000 },
    { name: '7d', used_percent: 31, resets_at: nowSec + (4 * 24 + 6) * 3600, window_secs: 604800 },
  ] } }] },
}

function breakdown(url: URL) {
  const by = url.searchParams.get('by')
  const all = sum(LOAD)
  const amount = total().amount_micro
  if (by === 'channel') {
    return { days: 7, total_amount_micro: amount, total_requests: all, data: CHANNELS.slice(0, 5).map((c, i) => ({
      rank: i + 1, key: String(c.id), label: c.name, channel_id: c.id, requests: CH_REQ[i], amount_micro: CH_REQ[i] * 6400,
      share_bp: Math.round((CH_REQ[i] / all) * 10000), tokens: CH_REQ[i] * 3780 })) }
  }
  return { days: 7, total_amount_micro: amount, total_requests: all, data: MODELS.map((m, i) => ({
    rank: i + 1, key: m, label: null, requests: Math.round(all * MODEL_SHARE[i]), amount_micro: Math.round(amount * MODEL_SHARE[i] * (i === 0 ? 1.25 : 0.9)),
    share_bp: Math.round(MODEL_SHARE[i] * 10000), tokens: Math.round(all * MODEL_SHARE[i] * 3840) })) }
}


// ---- portal
const PORTAL_MODELS = ['claude-sonnet-4-5', 'gpt-5.1', 'gemini-2.5-pro', 'deepseek-chat']
function portalBreakdown(url: URL) {
  const start = url.searchParams.get('start_date') ?? (url.searchParams.get('days') === '1' ? DAYS[6] : DAYS[0])
  const end = url.searchParams.get('end_date') ?? DAYS[6]
  const prev = end < DAYS[0]
  const days = DAYS.map((d) => prev ? shiftDay(d, -7) : d).filter((d) => d >= start && d <= end)
  const weights = [0.46, 0.28, 0.16, 0.1]
  const data = days.flatMap((day) => {
    const i = DAYS.indexOf(prev ? shiftDay(day, 7) : day)
    const n = Math.round((LOAD[i] / 52) * (prev ? 0.84 : 1))
    return PORTAL_MODELS.map((model, j) => {
      const r = Math.max(1, Math.round(n * weights[j]))
      const amount = Math.round(r * [9200, 6100, 4800, 900][j])
      return { day, model, requests: r, prompt_tokens: r * 4200, cached_tokens: r * 2300, cache_write_tokens: r * 160, completion_tokens: r * 720,
        reasoning_tokens: r * 180, amount_micro: amount, discount_micro: Math.round(amount * 0.05), original_micro: Math.round(amount * 1.05), errors: j === 2 && i === 3 ? 2 : 0,
        performance_requests: r, latency_sum_ms: r * (2000 + j * 300), ttft_sum_ms: r * (600 + j * 90), ttft_samples: r }
    })
  })
  const s = (k: string) => data.reduce((a, r) => a + Number((r as Record<string, number | string>)[k] ?? 0), 0)
  return { days: days.length, scope: url.searchParams.get('scope') ?? 'user', live: { rpm: 6, tpm: 31000, rpd: 212, rpm_limit: 60, tpm_limit: 400000, rpd_limit: 5000 },
    window: { start_date: start, end_date: end, today: DAYS[6], timezone: 'Asia/Shanghai', generated_at: '2026-11-20 15:30:00' }, data,
    total: { requests: s('requests'), prompt_tokens: s('prompt_tokens'), cached_tokens: s('cached_tokens'), cache_write_tokens: s('cache_write_tokens'), completion_tokens: s('completion_tokens'),
      reasoning_tokens: s('reasoning_tokens'), tokens: s('prompt_tokens') + s('completion_tokens'), amount_micro: s('amount_micro'), discount_micro: s('discount_micro'), cache_hit_bp: 5476,
      avg_rpm_micro: 160000, avg_tpm_micro: 820000000, success_rate_bp: 9994, avg_latency_ms: 2280, avg_ttft_ms: 690, ttft_samples: s('ttft_samples'), performance_requests: s('performance_requests'), tokens_per_1k_sec: 2100000 },
    wallet_window_spend_micro: s('amount_micro') }
}

// ---- logs: 最近的请求，第一条带完整计费快照（打开详情就是账单解释）
const SNAPSHOT = { mode: 'ratio', epoch: 12, model_ratio: '1.5', completion_ratio: '5', cache_ratio: '0.1', cache_write_ratio: '1.25', base_price_per_1m_usd: '2',
  final_unit_price_input_per_1m_usd: '2.295', group: 'pro', group_ratio: '0.85', user_multiplier: '1', rules: [{ code: 'off_peak', multiplier: '0.9' }] }
const logRow = (i: number, model: string, over: Json = {}) => ({
  id: 9000 - i, request_id: `7f3c${(9137 + i * 7919).toString(16)}-2b1e-4c55-9a0e-${(0x5d21a8c0 + i * 104729).toString(16)}d1`, upstream_request_id: `msg_01${(83311 + i).toString(36)}Qx`,
  model, requested_model: model, endpoint: model.startsWith('claude') ? '/v1/messages' : '/v1/chat/completions', log_type: 2, status: 20, api_key_id: 3, key_name: 'cursor-laptop', pool: 0,
  usage_details_recorded: true, usage: { prompt_tokens: 18240 - i * 610, cached_tokens: 12800 - i * 400, completion_tokens: 1260 + i * 37, reasoning_tokens: 320, cache_read_reported: true, cache_write_reported: true, cache_write_tokens: 640, input_unit: 'tokens',
    audio_prompt_tokens: 0, image_prompt_tokens: 0, audio_completion_tokens: 0, image_completion_tokens: 0, prompt_source: 'upstream', completion_source: 'upstream' },
  amount_micro: 21_380 - i * 900, original_amount_micro: 23_760 - i * 1000, discount_micro: 2_380 - i * 100, pricing_snapshot: null, error_code: null,
  latency_ms: 3820 - i * 120, ttft_ms: 610 + i * 13, is_stream: true, created_at: ago(2 + i * 3), ...over,
})
const LOGS = [
  logRow(0, 'claude-sonnet-4-5', { pricing_snapshot: SNAPSHOT, request_id: '7f3c2a91-2b1e-4c55-9a0e-5d21a8c0d1e4', amount_micro: 30_248, original_amount_micro: 39_540, discount_micro: 9_292 }),
  logRow(1, 'gpt-5.1'), logRow(2, 'claude-sonnet-4-5'), logRow(3, 'gemini-2.5-pro'),
  logRow(4, 'deepseek-chat', { amount_micro: 1_920, original_amount_micro: 2_020, discount_micro: 100 }),
  logRow(5, 'claude-sonnet-4-5', { log_type: 5, status: 30, error_code: 'upstream_timeout', amount_micro: 0, original_amount_micro: 0, discount_micro: 0, ttft_ms: null, latency_ms: 30000 }),
  logRow(6, 'gpt-5.1'), logRow(7, 'claude-sonnet-4-5'), logRow(8, 'gemini-2.5-pro'), logRow(9, 'gpt-5.1'), logRow(10, 'claude-sonnet-4-5'), logRow(11, 'deepseek-chat'),
].map((r) => ({ ...r, pricing_snapshot: r.pricing_snapshot ?? { ...SNAPSHOT, model_ratio: r.model.startsWith('gpt') ? '0.625' : r.model.startsWith('gemini') ? '0.625' : r.model.startsWith('deepseek') ? '0.14' : '1.5' } }))
const LOG_STATS = { records: 212, settled: 209, failed: 3, refunded: 1, pending: 0, errors: 3, amount_micro: 1_946_200, refunded_amount_micro: 0,
  prompt_tokens: 3_410_000, completion_tokens: 291_000, cached_tokens: 2_140_000, cache_read_samples: 212, avg_latency_ms: 3120, latency_samples: 212, avg_ttft_ms: 702, ttft_samples: 209,
  cache_write_tokens: 96_000, cache_write_samples: 212, reasoning_tokens: 41_000 }

// ---- public pricing catalog
const pm = (model: string, vendor: string, display: string, over: Json = {}) => ({ model, vendor, display_name: display, mode: 'ratio', model_ratio: '1', completion_ratio: '4', cache_ratio: '0.1',
  cache_write_ratio: '1.25', audio_ratio: null, audio_completion_ratio: null, image_ratio: null, per_call_price_micro: null, groups: ['default', 'pro'],
  capabilities: { tools: true, vision: true, reasoning: true }, context_window: 200000, max_output: 64000, ...over })
const CATALOG = [
  pm('claude-opus-4-1', 'Anthropic', 'Claude Opus 4.1', { model_ratio: '7.5', completion_ratio: '5', max_output: 32000 }),
  pm('claude-sonnet-4-5', 'Anthropic', 'Claude Sonnet 4.5', { model_ratio: '1.5', completion_ratio: '5' }),
  pm('claude-haiku-4-5', 'Anthropic', 'Claude Haiku 4.5', { model_ratio: '0.5', completion_ratio: '5' }),
  pm('gpt-5.1', 'OpenAI', 'GPT-5.1', { model_ratio: '0.625', completion_ratio: '8', context_window: 400000, max_output: 128000 }),
  pm('gpt-5-mini', 'OpenAI', 'GPT-5 mini', { model_ratio: '0.125', completion_ratio: '8', context_window: 400000, max_output: 128000 }),
  pm('o4-mini', 'OpenAI', 'o4-mini', { model_ratio: '0.55', completion_ratio: '4', capabilities: { tools: true, reasoning: true } }),
  pm('gemini-2.5-pro', 'Google', 'Gemini 2.5 Pro', { mode: 'tiered', model_ratio: '0.625', completion_ratio: '8', context_window: 1048576, capabilities: { tools: true, vision: true, reasoning: true, audio: true, video: true } }),
  pm('gemini-2.5-flash', 'Google', 'Gemini 2.5 Flash', { model_ratio: '0.15', completion_ratio: '8.33', context_window: 1048576 }),
  pm('deepseek-chat', 'DeepSeek', 'DeepSeek V3.2', { model_ratio: '0.14', completion_ratio: '1.5', context_window: 128000, max_output: 8192, capabilities: { tools: true } }),
  pm('deepseek-reasoner', 'DeepSeek', 'DeepSeek V3.2 Thinking', { model_ratio: '0.14', completion_ratio: '1.5', context_window: 128000, capabilities: { reasoning: true } }),
  pm('qwen3-coder', 'Alibaba', 'Qwen3 Coder', { model_ratio: '0.5', completion_ratio: '4', context_window: 262144, capabilities: { tools: true } }),
  pm('gpt-image-1', 'OpenAI', 'GPT Image 1', { mode: 'per_call', per_call_price_micro: 42000, capabilities: { vision: true }, context_window: null, max_output: null }),
]
const GROUPS = [{ code: 'default', name: '标准', ratio: '1', is_default: true }, { code: 'pro', name: '专业版', ratio: '0.85', self_select: true }]
const pageMeta = (total: number, limit: number, offset: number, count: number) => ({ total, limit, offset, has_more: offset + count < total, next_offset: offset + count < total ? offset + count : null })
function pricing(url: URL) {
  if (url.pathname === '/api/pricing/stats') {
    const vendors = [...new Set(CATALOG.map((m) => m.vendor.toLowerCase()))].map((vendor) => ({ vendor, count: CATALOG.filter((m) => m.vendor.toLowerCase() === vendor).length }))
    return { total: CATALOG.length, capabilities: ['tools', 'vision', 'reasoning', 'audio', 'video'], has_context: true, vendors, vendors_page: pageMeta(vendors.length, 20, 0, vendors.length), pricing_epoch: 12 }
  }
  const limit = Number(url.searchParams.get('limit') ?? 24), offset = Number(url.searchParams.get('offset') ?? 0)
  const batch = CATALOG.slice(offset, offset + limit)
  return { models: batch, groups: GROUPS, ...pageMeta(CATALOG.length, limit, offset, batch.length), groups_page: pageMeta(GROUPS.length, 20, 0, GROUPS.length), pricing_epoch: 12 }
}

// ---- 调度池：Claude 订阅账号池按额度窗口紧迫度排序，Dry-Run 给出候选顺序与被排除的原因
const POOLS = [
  { pool_code: 'default', description: '缺省池：新渠道自动加入，未指定池的分组走这里', routing_strategy: 'priority_weighted', fallback_pool_code: null,
    policy_version: 3, builtin: true, channel_count: 7, group_count: 2, key_count: 14, fallback_ref_count: 2 },
  { pool_code: 'claude-sub', description: 'Claude 订阅账号：先用快到期作废的额度，长上下文避开快见底的号', routing_strategy: 'expiry_urgency', fallback_pool_code: 'default',
    policy_version: 7, builtin: false, channel_count: 4, group_count: 1, key_count: 4, fallback_ref_count: 0 },
  { pool_code: 'low-latency', description: '交互类请求：按首字延迟选路，失败降级到缺省池', routing_strategy: 'least_latency', fallback_pool_code: 'default',
    policy_version: 2, builtin: false, channel_count: 3, group_count: 1, key_count: 6, fallback_ref_count: 0 },
]
const SUB_POLICY = {
  fallback_pools: ['default'],
  selection: { mode: 'expiry_urgency', respect_priority_layers: true, urgency: { short_window_weight: 1, long_window_weight: 2, heavy_prompt_tokens: 100000, heavy_min_headroom_pct: 20 } },
  affinity: { mode: 'prefer_warm', ttl_secs: 3600 },
  failover: { max_attempts: 3, max_same_channel_retries: 1, first_output_timeout_secs: 30 },
  guardrails: { max_cost_milli: 1500 },
  quality: { enabled: true, min_samples: 20, demote_error_rate_milli: 50, eject_error_rate_milli: 200, demote_latency_ms: 8000 },
  canary_percent: 10,
  rules: [{ name: 'long-context', when: { min_prompt_tokens: 100000 }, apply: { failover: { first_output_timeout_secs: 60 } } }],
}
const SUB_MEMBERS = [
  { channel_id: 11, name: 'claude-max-01', provider: 'anthropic_max', status: 1, priority: 10, priority_override: null, weight_override: null, role: 'primary', enabled: true, tags: ['max'], models: ['claude-sonnet-4-5', 'claude-opus-4-1'], active_keys: 1 },
  { channel_id: 12, name: 'claude-max-02', provider: 'anthropic_max', status: 1, priority: 10, priority_override: null, weight_override: null, role: 'primary', enabled: true, tags: ['max'], models: ['claude-sonnet-4-5', 'claude-opus-4-1'], active_keys: 1 },
  { channel_id: 13, name: 'claude-pro-03', provider: 'anthropic_max', status: 1, priority: 8, priority_override: null, weight_override: null, role: 'backup', enabled: true, tags: ['pro'], models: ['claude-sonnet-4-5'], active_keys: 1 },
  { channel_id: 14, name: 'claude-max-04', provider: 'anthropic_max', status: 1, priority: 10, priority_override: null, weight_override: null, role: 'canary', enabled: true, tags: ['max'], models: ['claude-sonnet-4-5'], active_keys: 1 },
]
const cand = (channel_id: number, channel_name: string, provider: string, pool_rank: number, pool_role: string, priority: number, over: Json = {}) =>
  ({ channel_id, channel_key_id: channel_id * 10 + 1, channel_name, provider, pool_rank, pool_role, priority, weight: 1, cost_milli: 1000, tags: [], ...over })
const SIMULATE = {
  pool_code: 'claude-sub', requested_model: 'claude-sonnet-4-5', canonical_model: 'claude-sonnet-4-5',
  saved: {
    policy_version: 7, canary_selected: false,
    effective_policy: { primary_pool: 'claude-sub', pool_chain: ['claude-sub', 'default'], strategy: 'expiry_urgency', respect_priority_layers: true, ignore_cost_in_weight: false,
      affinity_mode: 'prefer_warm', affinity_ttl_secs: 3600, max_attempts: 3, max_same_channel_retries: 1, first_output_timeout_secs: 60, guardrails: { max_cost_milli: 1500 },
      canary_percent: 10, matched_rules: ['long-context'] },
    ordered_candidates: [
      cand(12, 'claude-max-02', 'anthropic_max', 0, 'primary', 10, { tags: ['max'], urgency_score: 87, dominant_window: '7d', quality_status: 'healthy', latency_ms: 1120 }),
      cand(11, 'claude-max-01', 'anthropic_max', 0, 'primary', 10, { tags: ['max'], urgency_score: 64, dominant_window: '5h', quality_status: 'healthy', latency_ms: 1240 }),
      cand(13, 'claude-pro-03', 'anthropic_max', 0, 'backup', 8, { tags: ['pro'], urgency_score: 41, dominant_window: '5h', quality_status: 'demoted_latency', latency_ms: 9400 }),
      cand(1, 'anthropic-official', 'anthropic', 1, 'primary', 10, { cost_milli: 1200, quality_status: 'healthy', latency_ms: 812 }),
      cand(6, 'bedrock-us-west-2', 'bedrock', 1, 'primary', 4, { cost_milli: 1100, quality_status: 'healthy', latency_ms: 1043 }),
    ],
    rejected_candidates: [
      { channel_id: 14, channel_key_id: 141, channel_name: 'claude-max-04', provider: 'anthropic_max', pool_rank: 0, pool_role: 'canary', reason: 'canary_not_selected' },
      { channel_id: 3, channel_key_id: 31, channel_name: 'vertex-gemini', provider: 'vertex', pool_rank: 1, pool_role: 'primary', reason: 'member_model_filter' },
    ],
  },
  draft: null,
}

// ---- 模型验真：三层都通过的结果（协议 / 分词器 / 行为指纹），SSE 一次性回放
const BANK = { loaded: true, cache_exists: true, built_at: '2026-11-18T00:00:00Z', model_count: 75, tokenizer_classes: 62, tokenizer_probes: 72, sources: ['https://lm.ikale.io/data/'] }
function verifySse() {
  const protocol = {
    model_name: { declared_model: 'claude-sonnet-4-5', upstream_model: 'claude-sonnet-4-5', response_model: 'claude-sonnet-4-5-20250929', declared_family: 'claude', response_family: 'claude', status: 'pass' },
    tool_call: { status: 'pass', nonce: 'okapi-7c41e9a2', tool_called: true, nonce_matched: true, arguments_valid_json: true, latency_ms: 1380 },
    signature: { status: 'pass', applicable: true, has_thinking_block: true, has_signature: true, signature_len: 412, thinking_mode: 'enabled', latency_ms: 2210 },
  }
  const posterior = {
    classes: [
      { id: 'anthropic_claude', lab: 'anthropic', lab_name: 'Anthropic', series: 'Claude', series_zh: 'Claude', exact: 0.962, related: 0.021, matched: 11, compared: 12, overhead: 9 },
      { id: 'openai_o200k', lab: 'openai', lab_name: 'OpenAI', series: 'o200k_base', series_zh: 'o200k_base', exact: 0.009, related: 0.004, matched: 2, compared: 12, overhead: 18 },
    ],
    labs: [{ lab: 'anthropic', lab_name: 'Anthropic', total: 0.983 }, { lab: 'openai', lab_name: 'OpenAI', total: 0.013 }],
    unknown: 0.004, alien: 0.002, answered: 12,
  }
  const tokenizer = { posterior, probes_run: 12, baseline_tokens: 9, baseline_drift: 0, latency_ms: 6420,
    verdict: { kind: 'exact', top: posterior.classes[0], confidence: 0.962, claim: { expected: ['anthropic_claude'], vendor: 'Anthropic', first_party: true, probability: 0.983, status: 'consistent' } } }
  const samples = [0, 1, 2].map((index) => ({ index, expected_count: 260, parsed_numbers: 260, minimum_numbers: 182, accepted: true, latency_ms: 4100 + index * 380, response_model: 'claude-sonnet-4-5-20250929' }))
  const fingerprint = {
    bank_built_at: BANK.built_at, bank_model_count: 75, decision: 'scored', prediction: 'claude-sonnet-4.5', prediction_name: 'Claude Sonnet 4.5',
    family_prediction: 'claude', family_prediction_name: 'Claude', declared_family: 'claude', family_matches: true, declared_in_bank: true,
    top_probability: 0.91, top_family_probability: 0.98, probability_status: 'reference_calibrated', used_outputs: 3, model_like: true,
    evidence_label: 'Strong', evidence_reason: 'Top family dominates posterior.',
    top_matches: [
      { model: 'claude-sonnet-4.5', display_name: 'Claude Sonnet 4.5', family: 'claude', family_name: 'Claude', score: 0.93, probability: 0.91 },
      { model: 'claude-opus-4.1', display_name: 'Claude Opus 4.1', family: 'claude', family_name: 'Claude', score: 0.71, probability: 0.06 },
      { model: 'gpt-5.1', display_name: 'GPT-5.1', family: 'gpt', family_name: 'GPT', score: 0.38, probability: 0.02 },
    ],
    family_probabilities: [{ family: 'claude', family_name: 'Claude', probability: 0.98 }, { family: 'gpt', family_name: 'GPT', probability: 0.02 }],
    samples,
  }
  const report = { channel_id: 1, provider: 'anthropic', model: 'claude-sonnet-4-5', upstream_model: 'claude-sonnet-4-5', mimic_profile_active: false,
    verdict: 'verified', warnings: [], protocol, tokenizer, fingerprint, elapsed_ms: 19_840 }
  const ev = (data: Json) => `event: ${data.type}\ndata: ${JSON.stringify(data)}\n\n`
  return [
    ev({ type: 'init', channel_id: 1, provider: 'anthropic', model: 'claude-sonnet-4-5', upstream_model: 'claude-sonnet-4-5', mimic_profile_active: false,
      checks: { protocol: true, tokenizer: true, fingerprint: true }, bank: BANK }),
    ev({ type: 'protocol_result', result: protocol }),
    ev({ type: 'tokenizer_result', result: tokenizer }),
    ...samples.map((sample) => ev({ type: 'fingerprint_sample', sample })),
    ev({ type: 'fingerprint_result', result: fingerprint }),
    ev({ type: 'done', report }),
  ].join('')
}

// ---- 对话存档：一条 Claude Code 风格的对话（思考过程 + 工具调用），存在 ClickHouse
const ARCHIVE_ID = '5c0f6a1e-93d2-4b8e-a7c4-1f20e9b3d6a8'
const ARCHIVE_REQ = {
  model: 'claude-sonnet-4-5', max_tokens: 4096, stream: true,
  system: '你是代码助手，回答简洁，必要时调用工具读取文件。',
  tools: [{ name: 'read_file', description: '读取仓库里的文件', input_schema: { type: 'object', properties: { path: { type: 'string' } }, required: ['path'] } }],
  messages: [{ role: 'user', content: '帮我看看 src/billing.rs 里退款为什么会重复入账？' }],
}
const ARCHIVE_TOOLS = [{ id: 'toolu_01', name: 'read_file', arguments: '{"path":"src/billing.rs"}' }]
const ARCHIVE_TEXT = '问题在 refund() 没有按 request_id 去重：上游超时重试时同一笔请求会进来两次。建议在写退款事件前先查 refunded_at，或把 (request_id, kind) 设成唯一键，让第二次写入直接冲突返回。'
const ARCHIVE_THINK = '先读 billing.rs 的 refund 路径。重试来自网关 failover，同一个 request_id 可能两次触发退款；看有没有幂等键……'
const ARCHIVE = {
  request_id: ARCHIVE_ID, user_id: 7, api_key_id: 12, channel_id: 11, model: 'claude-sonnet-4-5', requested_model: 'claude-sonnet-4-5', endpoint: '/v1/messages',
  is_stream: true, status_code: 200, error_code: '', request_body: JSON.stringify(ARCHIVE_REQ),
  response_body: JSON.stringify({ stream: true, text: ARCHIVE_TEXT, reasoning: ARCHIVE_THINK, tool_calls: ARCHIVE_TOOLS }),
  response_text: ARCHIVE_TEXT, reasoning_text: ARCHIVE_THINK, tool_calls: JSON.stringify(ARCHIVE_TOOLS), truncated: false, created_at: ago(6), storage: 'clickhouse',
}
const archiveRow = (i: number, model: string, ask: string, reply: string, over: Json = {}) => ({
  request_id: `${(0x5c0f6a1e + i * 7919).toString(16)}-93d2-4b8e-a7c4-${(0x1f20e9b3d6a8 + i * 104729).toString(16)}`, user_id: 7 + (i % 3), username: ['dev-lin', 'dev-zhao', 'ops-wu'][i % 3],
  api_key_id: 12, channel_id: 11, channel_name: 'claude-max-01', model, requested_model: model, endpoint: model.startsWith('claude') ? '/v1/messages' : '/v1/chat/completions',
  is_stream: true, status_code: 200, error_code: '', request_preview: ask, response_preview: reply, truncated: false, created_at: ago(6 + i * 9), storage: 'clickhouse', ...over,
})
const ARCHIVES = [
  archiveRow(0, 'claude-sonnet-4-5', ARCHIVE_REQ.messages[0].content, ARCHIVE_TEXT, { request_id: ARCHIVE_ID }),
  archiveRow(1, 'gpt-5.1', '把这段 SQL 改成按月分区的写法', '可以用 PARTITION BY RANGE (created_at)，每月建一个子表……'),
  archiveRow(2, 'claude-sonnet-4-5', '解释一下这个 panic 的调用栈', 'panic 来自 unwrap() 拿到了 None：config.redis_url 没配……'),
  archiveRow(3, 'gemini-2.5-pro', '总结这份周报的三个风险点', '一是供应商额度紧张；二是 p95 延迟上升；三是退款积压……'),
  archiveRow(4, 'claude-opus-4-1', '帮我写个 Rust 的重试中间件，指数退避', '下面是一个基于 tower::Layer 的实现……', { truncated: true }),
  archiveRow(5, 'deepseek-chat', '这条正则为什么匹配不到中文', '\\w 默认只匹配 ASCII，换成 \\p{Han} 或开 unicode 标志……'),
]

// ---- 审计日志：谁在何时从哪改了什么；第一条带变更前后对比
const AUDIT = [
  { id: 5120, actor: 'admin:1', actor_info: { kind: 'admin', id: 1, label: 'admin' }, action: 'channel.update', target: '7',
    detail: { before: { priority: 3, weight: 1, pools: ['default'] }, after: { priority: 6, weight: 2, pools: ['default', 'low-latency'] } },
    ip: '203.0.113.24', user_agent: 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) Chrome/131.0', trace_id: '4bf92f3577b34da6a3ce929d0e0e4736', actor_key_id: null, created_at: ago(4) },
  { id: 5119, actor: 'admin:1', actor_info: { kind: 'admin', id: 1, label: 'admin' }, action: 'pricing.publish', target: 'epoch:12',
    detail: { epoch: 12, changed_models: 3 }, ip: '203.0.113.24', user_agent: 'Mozilla/5.0 Chrome/131.0', trace_id: '9a1c4e2f6b7d48e0a3f5c2b1d0e9f8a7', actor_key_id: null, created_at: ago(22) },
  { id: 5118, actor: 'worker', actor_info: { kind: 'worker', id: null, label: null }, action: 'margin_breaker.trip', target: 'pro:8',
    detail: { group_code: 'pro', channel_id: 8, requests: 214, margin_bp: -180 }, ip: null, user_agent: null, trace_id: null, actor_key_id: null, created_at: ago(47) },
  { id: 5117, actor: 'mcp:3', actor_info: { kind: 'mcp', id: 3, label: 'admin / ops-agent' }, action: 'channel.set_status', target: '9',
    detail: { before: { status: 1 }, after: { status: 2 } }, ip: '198.51.100.12', user_agent: 'claude-code/2.1', trace_id: 'c2d1e0f9a8b7465c3d2e1f0a9b8c7d6e', actor_key_id: 3, created_at: ago(65) },
  { id: 5116, actor: 'admin:1', actor_info: { kind: 'admin', id: 1, label: 'admin' }, action: 'channel.delete', target: '15',
    detail: { deleted: { name: 'legacy-openai', provider: 'openai', models_count: 5 } }, ip: '203.0.113.24', user_agent: 'Mozilla/5.0 Chrome/131.0', trace_id: null, actor_key_id: null, created_at: ago(96) },
  { id: 5115, actor: 'user:42', actor_info: { kind: 'user', id: 42, label: 'dev-lin' }, action: 'key.rotate', target: 'key:88',
    detail: { name: 'cursor-laptop' }, ip: '192.0.2.61', user_agent: 'Mozilla/5.0 Safari/18.1', trace_id: null, actor_key_id: null, created_at: ago(130) },
  { id: 5114, actor: 'admin:1', actor_info: { kind: 'admin', id: 1, label: 'admin' }, action: 'settings.set', target: 'conversation_archive',
    detail: { before: { enabled: false }, after: { enabled: true, retention_days: 30 } }, ip: '203.0.113.24', user_agent: 'Mozilla/5.0 Chrome/131.0', trace_id: null, actor_key_id: null, created_at: ago(185) },
  { id: 5113, actor: 'anon', actor_info: null, action: 'user.login_failed', target: 'dev-zhao@example.com', detail: { reason: 'bad_password' },
    ip: '192.0.2.200', user_agent: 'curl/8.7.1', trace_id: null, actor_key_id: null, created_at: ago(240) },
  { id: 5112, actor: 'user:43', actor_info: { kind: 'user', id: 43, label: 'dev-zhao' }, action: 'user.login', target: 'dev-zhao@example.com', detail: { method: 'password' },
    ip: '192.0.2.200', user_agent: 'Mozilla/5.0 Firefox/133.0', trace_id: null, actor_key_id: null, created_at: ago(241) },
]

const ME = { user_id: 1, key_id: 1, role: 100, group: 'default', balance_micro: 1_286_400_000, permissions: ['*'], username: 'admin', email: 'admin@example.com', balance_expires_at: null }

const shiftDay = (d: string, n: number) => new Date(Date.parse(`${d}T00:00:00Z`) + n * 86400_000).toISOString().slice(0, 10)
// 上一期（end_date 早于本期）：同形数据打 86 折，环比就不是 0%
const previousWindow = (url: URL) => (url.searchParams.get('end_date') ?? DAYS[6]) < DAYS[0]
const scaledTrend = (url: URL): typeof TREND => {
  if (!previousWindow(url)) return TREND
  return TREND.map((r, i) => {
    // 各指标涨幅不同：请求 +16% 上下、金额 +24% 上下、Token +11% 上下
    const f = (k: string) => (k.includes('amount') || k.includes('cost') ? 0.78 : k.includes('token') ? 0.88 : 0.84) + (i % 3) * 0.02
    const scaled = Object.fromEntries(Object.entries(r).map(([k, v]) => [k, typeof v === 'number' && !k.startsWith('avg') && !k.endsWith('_bp') ? Math.round(v * f(k)) : v]))
    return { ...r, ...scaled, bucket: shiftDay(DAYS[i], -7) }
  })
}

function fixture(url: URL): unknown {
  const p = url.pathname
  const days = Number(url.searchParams.get('days') ?? 7)
  const quotaId = /^\/admin\/channels\/(\d+)\/usage$/.exec(p)?.[1]
  if (quotaId !== undefined) return QUOTAS[quotaId] ?? { timezone: 'UTC', quotas: [] }
  switch (p) {
    case '/api/me': return ME
    case '/api/notice': return { notice: null }
    case '/api/notices': return { data: [] }
    case '/admin/diagnose': return { postgres: true, redis: true, clickhouse: true, nats_connected: true, outbox_pending: 0, dlq_depth: 0, cooling_keys: 1, pricebook_epoch: 12 }
    case '/admin/reconciliation': return { drift_count: 0, drifts: [] }
    case '/admin/stats/overview': {
      const b = (r: (typeof TREND)[number]) => ({ requests: r.requests, tokens: r.tokens, amount_micro: r.amount_micro, errors: r.errors, error_rate_bp: r.error_rate_bp, active_users: r.active_users })
      const rows = scaledTrend(url), t = total(rows)
      return { days, calendar: calendar(), today: b(rows[6]), yesterday: b(rows[5]),
        window: { requests: t.requests, tokens: t.tokens, amount_micro: t.amount_micro, errors: t.errors, error_rate_bp: t.error_rate_bp, active_users: 63 } }
    }
    case '/admin/stats/trend': return { days: 7, granularity: 'day', scope: {}, window: { ...calendar(), start_at: `${DAYS[0]} 00:00:00`, end_at: `${DAYS[6]} 23:59:59`,
      freshness: { last_event_at: ago(0.3), last_ingested_at: ago(0.2), pending_events: 0, failed_events: 0, queue_age_seconds: 5, event_gap_seconds: 5, stale: false, checked_at: ago(0) } },
      data: scaledTrend(url), total: total(scaledTrend(url)), previous: total(scaledTrend(new URL('http://x/?end_date=2026-11-13'))) }
    case '/admin/stats/breakdown': return breakdown(url)
    case '/admin/stats/margin': return { days, window: calendar(), total: total(), data: TREND.map((r) => ({ day: r.bucket, requests: r.requests, amount_micro: r.amount_micro, discount_micro: r.discount_micro, upstream_cost_micro: r.upstream_cost_micro })) }
    case '/admin/stats/realtime': return { window_secs: 60, qps_milli: 2140, requests: 128, errors: 1, error_rate_bp: 78, tokens: 486_000, amount_micro: 820_000,
      series: Array.from({ length: 60 }, (_, i) => ({ ts: nowSec - 60 + i, requests: 1 + ((i * 7) % 5), errors: i === 41 ? 1 : 0, tokens: 0, amount_micro: 0 })) }
    case '/admin/stats/inventory': return { users: { total: 186, active: 63, new_today: 4, new_7d: 21 }, api_keys: { total: 412, active: 377, used_7d: 158 },
      channels: { total: 9, healthy: 8, no_key: 0, auto_disabled: 0 }, models: { total: 48, priced: 48, served: 31 } }
    case '/admin/stats/cashflow': return { today: { recharge_micro: 120_000_000, granted_micro: 5_000_000, clawed_micro: 0, expired_micro: 0 }, window: { recharge_micro: 860_000_000, granted_micro: 40_000_000, clawed_micro: 0, expired_micro: 2_000_000 } }
    case '/admin/channels': return { data: CHANNELS, total: CHANNELS.length, enabled: CHANNELS.filter((c) => c.status === 1).length }
    case '/admin/channels/providers': return PROVIDERS
    case '/admin/stats/channels': return CHANNEL_HEALTH
    case '/admin/stats/channel-usage': return CHANNEL_USAGE
    case '/admin/proxies': return { data: [{ id: 1, name: 'sg-egress-01', status: 1 }], total: 1 }
  }
  if (p === '/admin/pools') return { data: POOLS, total: POOLS.length }
  if (p === '/admin/pools/claude-sub') return { ...POOLS[1], policy: SUB_POLICY, members: SUB_MEMBERS, models: ['claude-sonnet-4-5', 'claude-opus-4-1'], groups: ['pro'] }
  if (p === '/admin/channels/verify/bank') return BANK
  if (p === '/admin/audit') return { data: AUDIT, total: 1286, has_more: true, next_before: null }
  if (p === '/admin/audit/actions') return { data: [...new Set(AUDIT.map((r) => r.action))] }
  if (p === '/admin/conversations') return { backend: 'clickhouse', clickhouse_enabled: true, data: ARCHIVES }
  if (p === '/admin/conversations/stats') return { backend: 'clickhouse', clickhouse_enabled: true, total_count: 18_432, pg_count: 0, ch_count: 18_432,
    oldest_at: '2026-10-21T00:00:00Z', newest_at: ago(6), policy: { enabled: true, retention_days: 30, allow_user_view: true, max_body_kb: 1024, cleanup_interval_hours: 6 } }
  if (p === `/admin/logs/${ARCHIVE_ID}/conversation`) return { data: ARCHIVE }
  if (p === '/admin/settings/conversation_archive') return { value: { enabled: true, retention_days: 30, allow_user_view: true, max_body_kb: 1024, cleanup_interval_hours: 6 } }
  if (p === '/api/me/stats/breakdown') return portalBreakdown(url)
  if (p === '/api/me/logs') return { scope: 'user', data: LOGS, next_before: null }
  if (p === '/api/me/logs/stat') return LOG_STATS
  if (p === '/api/pricing' || p === '/api/pricing/stats') return pricing(url)
  if (p === '/api/me/groups') return { current: 'pro', data: GROUPS.map((g) => ({ code: g.code })) }
  if (p.startsWith('/admin/settings/')) return { value: null }
  unknown.add(p + (url.search ? ` ${url.search.slice(0, 80)}` : ''))
  return { data: [], total: 0 }
}

async function prepare(page: Page, { dark = false } = {}) {
  pageErrors.length = 0
  page.on('pageerror', (error) => pageErrors.push(error.message))
  page.on('console', (message) => { if (message.type() === 'error') pageErrors.push(message.text()) })
  await page.addInitScript((dark) => {
    localStorage.setItem('okapi.key', 'readme-demo-key')
    localStorage.setItem('okapi.lang', 'zh-CN')
    localStorage.setItem('okapi.theme', dark ? 'dark' : 'light')
    localStorage.setItem('okapi.guide.1', 'dismissed')
  }, dark)
  await page.clock.setFixedTime(NOW)
  await page.emulateMedia({ reducedMotion: 'reduce', colorScheme: dark ? 'dark' : 'light' })
  await page.route('**/*', async (route: Route) => {
    const request = route.request()
    const url = new URL(request.url())
    if (request.isNavigationRequest()) return route.fulfill({ path: fileURLToPath(new URL('../dist/index.html', import.meta.url)), contentType: 'text/html' })
    if (!/^\/(api|admin|auth|pay)\//.test(url.pathname)) return route.continue()
    if (request.method() === 'POST' && url.pathname === '/admin/pools/claude-sub/simulate') return route.fulfill({ json: SIMULATE })
    if (request.method() === 'POST' && url.pathname === '/admin/channels/1/verify') {
      return route.fulfill({ status: 200, contentType: 'text/event-stream; charset=utf-8', body: verifySse() })
    }
    return route.fulfill({ json: fixture(url) })
  })
}

// 截图前确认页面没报错、没落到错误边界：桩的形状不对时宁可失败，也别截出一张坏图
const pageErrors: string[] = []
async function snap(page: Page, name: string, full = false) {
  await expect(page.getByText('Something went wrong!')).toHaveCount(0)
  expect(pageErrors, '页面报错').toEqual([])
  await page.evaluate(() => document.fonts.ready)
  await page.waitForTimeout(900)
  await page.screenshot({ path: `${OUT}${name}.png`, fullPage: full, animations: 'disabled' })
}

test.afterAll(() => {
  if (unknown.size) console.log('UNSTUBBED:\n  ' + [...unknown].sort().join('\n  '))
})

test('admin dashboard', async ({ page }) => {
  await prepare(page)
  await page.goto('/admin')
  await expect(page.locator('[data-slot="dashboard-trend"]')).toBeVisible()
  await snap(page, 'admin-dashboard')
})

test('channels', async ({ page }) => {
  await prepare(page)
  // 卡片视图一屏四条：置顶订阅号（额度窗口）、有 key 冷却的 deepseek（余额）、正在出错的 qwen，再接正常的官方渠道
  const pinned = [4, 7, 8]
  await page.route((url) => url.pathname === '/admin/channels', (route) => {
    if (route.request().isNavigationRequest()) return route.fallback()
    const byId = (id: number) => CHANNELS.find((c) => c.id === id) as Json
    const rows = [...pinned.map((id, i) => ({ ...byId(id), pinned: true, pinned_at: ago(600 + i) })), ...CHANNELS.filter((c) => !pinned.includes(Number(c.id)))]
    return route.fulfill({ json: { data: rows, total: rows.length, enabled: rows.filter((c) => c.status === 1).length } })
  })
  await page.goto('/admin/channels')
  await expect(page.locator('#main-content').getByRole('heading', { name: '渠道' })).toBeVisible()
  await page.getByRole('button', { name: '卡片' }).click()
  await expect(page.getByText('qwen-dashscope').first()).toBeVisible()
  await snap(page, 'admin-channels')
})

test('admin analytics', async ({ page }) => {
  await prepare(page)
  await page.goto('/admin/stats')
  await page.waitForTimeout(1500)
  await snap(page, 'admin-analytics')
})

test('portal overview', async ({ page }) => {
  await prepare(page)
  await page.goto('/portal')
  await page.waitForTimeout(1200)
  await snap(page, 'portal-overview')
})

test('portal log with bill explanation', async ({ page }) => {
  await prepare(page)
  await page.goto('/portal/logs?scope=user')
  await page.getByRole('button', { name: `展开 ${LOGS[0].request_id} 的明细`, exact: true }).click()
  const drawer = page.getByRole('dialog', { name: '请求与账单详情' })
  await expect(drawer).toBeVisible()
  await drawer.getByRole('heading', { name: '费用与计价依据' }).scrollIntoViewIfNeeded()
  await drawer.getByRole('heading', { name: '费用与计价依据' }).evaluate((node) => node.closest('[data-slot="detail-section"]')?.scrollIntoView({ block: 'start' }))
  await snap(page, 'portal-log-bill')
})

test('public pricing', async ({ page }) => {
  await prepare(page)
  await page.goto('/pricing')
  await page.waitForTimeout(1200)
  await snap(page, 'public-pricing')
})

test('admin dashboard dark', async ({ page }) => {
  await prepare(page, { dark: true })
  await page.goto('/admin')
  await expect(page.locator('[data-slot="dashboard-trend"]')).toBeVisible()
  await snap(page, 'admin-dashboard-dark')
})

test('routing pool dry-run', async ({ page }) => {
  await prepare(page)
  await page.goto('/admin/pools')
  await page.getByRole('row').filter({ hasText: 'claude-sub' }).getByRole('button', { name: '策略模拟' }).click()
  const drawer = page.getByRole('dialog')
  await expect(drawer).toBeVisible()
  await drawer.locator('#sim-model').fill('claude-sonnet-4-5')
  await drawer.locator('#sim-tokens').fill('128000')
  await drawer.getByRole('button', { name: '运行模拟' }).click()
  await expect(drawer.getByText('claude-max-02').first()).toBeVisible()
  await snap(page, 'admin-pool-simulate')
})

test('model verification', async ({ page }) => {
  await prepare(page)
  await page.goto('/admin/channels')
  await page.getByRole('button', { name: '更多操作' }).first().click()
  await page.getByRole('menuitem', { name: '模型验真' }).click()
  const drawer = page.getByRole('dialog')
  await expect(drawer).toBeVisible()
  await drawer.getByRole('button', { name: '开始验真' }).click()
  const verdict = drawer.locator('[data-slot="verify-verdict"]')
  await expect(verdict).toBeVisible()
  // 结论放到抽屉顶端，下面接着三层结果
  await verdict.evaluate((node) => node.scrollIntoView({ block: 'start' }))
  await snap(page, 'admin-model-verify')
})

test('conversation archive', async ({ page }) => {
  await prepare(page)
  await page.goto('/admin/logs')
  await page.getByRole('button', { name: '对话存档' }).click()
  const tab = page.locator('[data-slot="admin-conversations-tab"]')
  await expect(tab).toBeVisible()
  await tab.getByText(ARCHIVE_TEXT.slice(0, 20)).first().click()
  const viewer = page.getByRole('dialog').locator('[data-slot="conversation-viewer"]')
  await expect(viewer).toBeVisible()
  await viewer.getByText('思考过程（点击展开）').click()
  await snap(page, 'admin-conversation-archive')
})

test('audit log', async ({ page }) => {
  await prepare(page)
  await page.goto('/admin/audit')
  await expect(page.getByRole('navigation', { name: '分页' })).toBeVisible()
  await expect(page.getByRole('row').filter({ hasText: 'channel.update' })).toBeVisible()
  await snap(page, 'admin-audit')
})

