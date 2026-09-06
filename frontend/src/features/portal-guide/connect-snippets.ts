import { buildRequestExample, normalizeApiBase, shellQuote } from '@/features/public-pricing/request-examples'

// 按客户端而非按模型给配置：模型广场的"调用示例"回答"这个模型怎么调"，
// 这里回答"我手里的工具怎么接上本站"。地址归一与 cURL 生成复用 request-examples。
export const CLIENTS = ['curl', 'python', 'node', 'claude', 'codex', 'apps'] as const
export type ClientId = (typeof CLIENTS)[number]
export type CodeClient = Exclude<ClientId, 'apps'>

export const ENV_KEY = 'OKAPI_API_KEY'
export const KEY_PLACEHOLDER = 'YOUR_API_KEY'
const MODEL_PLACEHOLDER = 'MODEL_ID'

export interface ConnectConfig {
  /// 以 /v1 结尾的 OpenAI 兼容地址。
  base: string
  /// 去掉 /v1 的站点地址：Anthropic 系客户端自己拼 /v1/messages，给带 /v1 的会变成 /v1/v1。
  origin: string
  key: string
  model: string
}

export interface Snippet {
  /// 终端命令，或要写入的文件（`path` 原样展示，不翻译）。
  target: 'terminal' | 'file'
  path?: string
  code: string
}

export function resolveConnectConfig(rawBase: string, model: string, apiKey?: string): ConnectConfig | null {
  const base = normalizeApiBase(rawBase)
  if (!base) return null
  return {
    base,
    origin: base.slice(0, -'/v1'.length),
    key: apiKey ?? KEY_PLACEHOLDER,
    model: model.trim() || MODEL_PLACEHOLDER,
  }
}

const q = (value: string) => JSON.stringify(value)

/// 终端里设置密钥：脚本类片段一律从环境变量读，密钥不进文件、不进历史记录里的命令体。
export function keyExport(cfg: ConnectConfig): Snippet {
  return { target: 'terminal', code: `export ${ENV_KEY}=${shellQuote(cfg.key)}` }
}

function claudeEnv(cfg: ConnectConfig): Record<string, string> {
  // 四个档位全部指向所选模型：Claude Code 的后台/子代理任务缺省去请求 haiku，
  // 本站未必配了那个名字，与其让它 404 不如全部走用户选的模型。
  return {
    ANTHROPIC_BASE_URL: cfg.origin,
    ANTHROPIC_AUTH_TOKEN: cfg.key,
    ANTHROPIC_MODEL: cfg.model,
    ANTHROPIC_DEFAULT_OPUS_MODEL: cfg.model,
    ANTHROPIC_DEFAULT_SONNET_MODEL: cfg.model,
    ANTHROPIC_DEFAULT_HAIKU_MODEL: cfg.model,
  }
}

export function buildSnippets(client: CodeClient, cfg: ConnectConfig, prompt: string): Snippet[] {
  switch (client) {
    case 'curl': {
      const example = buildRequestExample(cfg.base, 'chat', cfg.model, prompt, false)
      return [keyExport(cfg), { target: 'terminal', code: example?.curl ?? '' }]
    }
    case 'python':
      return [keyExport(cfg), {
        target: 'file',
        path: 'main.py',
        code: [
          '# pip install openai',
          'import os',
          'from openai import OpenAI',
          '',
          `client = OpenAI(base_url=${q(cfg.base)}, api_key=os.environ[${q(ENV_KEY)}])`,
          'response = client.chat.completions.create(',
          `    model=${q(cfg.model)},`,
          `    messages=[{"role": "user", "content": ${q(prompt)}}],`,
          ')',
          'print(response.choices[0].message.content)',
        ].join('\n'),
      }]
    case 'node':
      return [keyExport(cfg), {
        target: 'file',
        path: 'main.mjs',
        code: [
          '// npm install openai',
          "import OpenAI from 'openai'",
          '',
          `const client = new OpenAI({ baseURL: ${q(cfg.base)}, apiKey: process.env.${ENV_KEY} })`,
          'const response = await client.chat.completions.create({',
          `  model: ${q(cfg.model)},`,
          `  messages: [{ role: 'user', content: ${q(prompt)} }],`,
          '})',
          'console.log(response.choices[0].message.content)',
        ].join('\n'),
      }]
    case 'claude': {
      const env = claudeEnv(cfg)
      return [
        {
          target: 'terminal',
          code: [...Object.entries(env).map(([name, value]) => `export ${name}=${shellQuote(value)}`), 'claude'].join('\n'),
        },
        { target: 'file', path: '~/.claude/settings.json', code: JSON.stringify({ env }, null, 2) },
      ]
    }
    case 'codex':
      return [keyExport(cfg), {
        target: 'file',
        path: '~/.codex/config.toml',
        code: [
          'model_provider = "okapi"',
          `model = ${q(cfg.model)}`,
          '',
          '[model_providers.okapi]',
          'name = "Okapi"',
          `base_url = ${q(cfg.base)}`,
          `env_key = ${q(ENV_KEY)}`,
          'wire_api = "responses"',
          'requires_openai_auth = false',
        ].join('\n'),
      }]
  }
}
