import type { ConnectConfig } from './connect-snippets'

/// 聊天客户端一键导入链接（IMPLEMENTATION §11.39；new-api"导入配置到聊天应用"的对应物）。
///
/// 只对带真实密钥的配置生成：链接里必然带 key，占位符没有意义。协议形状各家自己定，
/// 这里只负责拼对——它们都是纯字符串，e2e 直接断言形状。
export type ImportTarget = 'ccswitch-claude' | 'ccswitch-codex' | 'nextchat' | 'cherry'

export interface ImportLink {
  target: ImportTarget
  href: string
  /// 自有协议（`ccswitch://` / `cherrystudio://`）：浏览器交给系统处理，不开新标签页。
  scheme: boolean
}

const utf8ToBase64 = (text: string) => {
  const bytes = new TextEncoder().encode(text)
  let bin = ''
  for (const b of bytes) bin += String.fromCharCode(b)
  return btoa(bin)
}

/// cc-switch deep link（v1）：`resource=provider&app=claude|codex&name&endpoint&apiKey`。
/// Claude Code 走不带 /v1 的基址（它自己拼 /v1/messages），Codex 走带 /v1 的基址。
function ccSwitch(app: 'claude' | 'codex', cfg: ConnectConfig, siteName: string): string {
  const params = new URLSearchParams({
    resource: 'provider',
    app,
    name: siteName,
    endpoint: app === 'claude' ? cfg.origin : cfg.base,
    apiKey: cfg.key,
  })
  if (cfg.model !== 'MODEL_ID') params.set('model', cfg.model)
  return `ccswitch://v1/import?${params.toString()}`
}

/// NextChat（ChatGPT-Next-Web）：`/#/?settings={"key","url"}`；url 为不带 /v1 的站点地址。
function nextChat(cfg: ConnectConfig, appUrl: string): string {
  const settings = JSON.stringify({ key: cfg.key, url: cfg.origin })
  return `${appUrl.replace(/\/+$/, '')}/#/?settings=${encodeURIComponent(settings)}`
}

/// Cherry Studio：`cherrystudio://providers/api-keys?v=1&data=<base64 JSON>`，OpenAI 兼容类型。
function cherry(cfg: ConnectConfig, siteName: string): string {
  const data = {
    id: `okapi-${siteName.toLowerCase().replace(/[^a-z0-9]+/g, '-')}`,
    name: siteName,
    type: 'openai',
    baseUrl: cfg.base,
    apiKey: cfg.key,
  }
  return `cherrystudio://providers/api-keys?v=1&data=${encodeURIComponent(utf8ToBase64(JSON.stringify(data)))}`
}

export function buildImportLinks(cfg: ConnectConfig, siteName: string, nextChatUrl = 'https://app.nextchat.club'): ImportLink[] {
  return [
    { target: 'ccswitch-claude', href: ccSwitch('claude', cfg, siteName), scheme: true },
    { target: 'ccswitch-codex', href: ccSwitch('codex', cfg, siteName), scheme: true },
    { target: 'nextchat', href: nextChat(cfg, nextChatUrl), scheme: false },
    { target: 'cherry', href: cherry(cfg, siteName), scheme: true },
  ]
}
