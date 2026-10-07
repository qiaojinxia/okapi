import { getKey } from '@/lib/api'
import { isRecord } from './setting-catalog'

// 导出的是模板，不读登录凭证，也不把密钥放进提示词或剪贴板。
export function mcpConfig(endpoint: string): string {
  return JSON.stringify({
    mcpServers: {
      okapi: {
        type: 'http',
        url: endpoint,
        headers: { Authorization: 'Bearer <OKAPI_API_KEY>' },
      },
    },
  }, null, 2)
}

export interface McpConnection {
  server: string
  version: string
  protocol: string
  tools: { name: string; description: string }[]
}

type ConnectionFailure = 'auth' | 'forbidden' | 'unavailable' | 'response' | 'timeout' | 'network'
export class McpConnectionError extends Error {
  readonly reason: ConnectionFailure
  constructor(reason: ConnectionFailure) {
    super(reason)
    this.reason = reason
  }
}

// 只探测同源的握手与工具发现；不接受任意 URL，避免将登录凭证发送到第三方。
// 这是服务端可达性诊断，不等价于外部 AI 客户端已配置成功。
export async function testMcpConnection(): Promise<McpConnection> {
  const key = getKey()
  if (!key) throw new McpConnectionError('auth')
  const signal = AbortSignal.timeout(10_000)
  async function rpc(id: number, method: string, params?: unknown): Promise<Record<string, unknown>> {
    const response = await fetch('/mcp', {
      method: 'POST',
      redirect: 'error',
      // 网页登录的 key 绑定登录会话，只在带着会话 cookie 时有效（同源才会带上）；
      // 外部 AI 客户端用的是用户自己建的 API key，与这里的诊断无关
      credentials: 'same-origin',
      signal,
      headers: {
        Authorization: `Bearer ${key}`,
        'Content-Type': 'application/json',
        Accept: 'application/json, text/event-stream',
      },
      body: JSON.stringify({ jsonrpc: '2.0', id, method, ...(params === undefined ? {} : { params }) }),
    })
    if (!response.ok) {
      throw new McpConnectionError(response.status === 401 ? 'auth'
        : response.status === 403 ? 'forbidden' : 'unavailable')
    }
    if (!response.headers.get('content-type')?.includes('application/json')) throw new McpConnectionError('response')
    let data: unknown
    try { data = await response.json() } catch { throw new McpConnectionError('response') }
    if (!isRecord(data) || data.jsonrpc !== '2.0' || data.id !== id || data.error || !isRecord(data.result)) {
      throw new McpConnectionError('response')
    }
    return data.result
  }
  try {
    const init = await rpc(1, 'initialize', {
      protocolVersion: '2025-06-18', capabilities: {}, clientInfo: { name: 'okapi-console-check', version: '1.0' },
    })
    if (!isRecord(init.serverInfo) || typeof init.serverInfo.name !== 'string'
      || typeof init.serverInfo.version !== 'string' || typeof init.protocolVersion !== 'string'
      || !isRecord(init.capabilities) || !isRecord(init.capabilities.tools)) throw new McpConnectionError('response')
    const result = await rpc(2, 'tools/list')
    if (!Array.isArray(result.tools) || !result.tools.every((tool) => isRecord(tool)
      && typeof tool.name === 'string' && (tool.description === undefined || typeof tool.description === 'string'))) {
      throw new McpConnectionError('response')
    }
    return {
      server: init.serverInfo.name, version: init.serverInfo.version, protocol: init.protocolVersion,
      tools: result.tools.map((tool) => ({ name: tool.name, description: tool.description ?? '' })),
    }
  } catch (error) {
    if (signal.aborted) throw new McpConnectionError('timeout')
    if (error instanceof McpConnectionError) throw error
    throw new McpConnectionError('network')
  }
}
