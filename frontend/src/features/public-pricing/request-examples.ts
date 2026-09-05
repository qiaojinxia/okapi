// 端点与请求形状以 bins/okapi/src/gateway 的路由/探针为准。
// 这是用户选择的接口模板，不把计价方式或厂商名称当成模型能力。
export const requestTemplates = [
  { id: 'chat', name: 'Chat Completions', path: '/chat/completions', stream: true },
  { id: 'responses', name: 'Responses', path: '/responses', stream: true },
  { id: 'messages', name: 'Messages', path: '/messages', stream: true },
  { id: 'embeddings', name: 'Embeddings', path: '/embeddings', stream: false },
  { id: 'images', name: 'Images', path: '/images/generations', stream: false },
  { id: 'rerank', name: 'Rerank', path: '/rerank', stream: false },
  { id: 'speech', name: 'Speech', path: '/audio/speech', stream: false },
  { id: 'videos', name: 'Videos', path: '/videos', stream: false },
] as const
export type RequestTemplate = typeof requestTemplates[number]['id']

export function normalizeApiBase(raw: string): string | null {
  try {
    const url = new URL(raw.trim())
    if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password || url.search || url.hash) return null
    const path = url.pathname.replace(/\/+$/, '')
    url.pathname = path.endsWith('/v1') ? path : `${path}/v1`
    return url.toString().replace(/\/$/, '')
  } catch { return null }
}

export function defaultApiBase(origin: string, configured?: string): string {
  if (configured?.trim()) return configured.trim()
  const url = new URL(origin)
  const loopback = ['localhost', '127.0.0.1', '[::1]'].includes(url.hostname)
  // 仓库默认 console:8081、gateway:8080；Vite 本地预览也使用该网关。
  // 正式同域部署由 deploy/nginx-sse.conf 将 /v1 交给 gateway。
  if (url.port === '8081' || (loopback && ['5173', '4173', '4175'].includes(url.port))) url.port = '8080'
  return `${url.origin}/v1`
}

export function shellQuote(value: string): string {
  return `'${value.replace(/'/g, `'"'"'`)}'`
}

export function buildRequestExample(base: string, template: RequestTemplate, model: string, prompt: string, stream: boolean) {
  const normalized = normalizeApiBase(base)
  if (!normalized) return null
  const endpoint = requestTemplates.find((item) => item.id === template)
  if (!endpoint) return null
  const url = `${normalized}${endpoint.path}`
  const streaming = stream && endpoint.stream
  const payload: Record<string, unknown> = { model }
  switch (template) {
    case 'chat': Object.assign(payload, { messages: [{ role: 'user', content: prompt }], stream: streaming }); break
    case 'responses': Object.assign(payload, { input: prompt, stream: streaming }); break
    case 'messages': Object.assign(payload, { max_tokens: 1024, messages: [{ role: 'user', content: prompt }], stream: streaming }); break
    case 'embeddings': Object.assign(payload, { input: prompt }); break
    case 'images': Object.assign(payload, { prompt, n: 1 }); break
    case 'rerank': Object.assign(payload, { query: prompt, documents: ['Example document A', 'Example document B'], top_n: 2 }); break
    case 'speech': Object.assign(payload, { input: prompt, voice: 'alloy', response_format: 'mp3' }); break
    case 'videos': Object.assign(payload, { prompt, seconds: '4' }); break
  }
  const body = JSON.stringify(payload, null, 2)
  const headers = ['  --header "Authorization: Bearer ${OKAPI_API_KEY:?Set OKAPI_API_KEY first}"', "  --header 'Content-Type: application/json'"]
  if (template === 'messages') headers.push("  --header 'anthropic-version: 2023-06-01'")
  const curl = [
    `curl --fail-with-body --show-error${streaming ? ' --no-buffer' : ''} --request POST ${shellQuote(url)}`,
    ...headers,
    ...(template === 'speech' ? ["  --output 'speech.mp3'"] : []),
    `  --data-raw ${shellQuote(body)}`,
  ].join(' \\\n')
  const headerObject: Record<string, string> = { 'Content-Type': 'application/json' }
  if (template === 'messages') headerObject['anthropic-version'] = '2023-06-01'
  // 双层 JSON 序列化让模型名/提示词成为 Python 字符串中的数据，不拼入可执行代码。
  const python = [
    'import json', 'import os', 'import urllib.request', ...(template === 'speech' ? ['from pathlib import Path'] : []), '',
    `payload = json.loads(${JSON.stringify(JSON.stringify(payload))})`,
    `headers = ${JSON.stringify(headerObject)}`,
    'headers["Authorization"] = "Bearer " + os.environ["OKAPI_API_KEY"]',
    'request = urllib.request.Request(', `    ${JSON.stringify(url)},`,
    '    data=json.dumps(payload).encode("utf-8"), headers=headers, method="POST"', ')',
    'with urllib.request.urlopen(request) as response:',
    ...(template === 'speech' ? ['    Path("speech.mp3").write_bytes(response.read())'] : streaming
      ? ['    for line in response:', '        print(line.decode("utf-8"), end="", flush=True)']
      : ['    print(response.read().decode("utf-8"))']),
  ].join('\n')
  const javascript = [
    ...(template === 'speech' ? ["import { writeFile } from 'node:fs/promises'", ''] : []),
    "const apiKey = process.env.OKAPI_API_KEY", "if (!apiKey) throw new Error('Set OKAPI_API_KEY first')", '',
    `const payload = ${body}`, `const response = await fetch(${JSON.stringify(url)}, {`,
    "  method: 'POST',", `  headers: { ...${JSON.stringify(headerObject)}, Authorization: 'Bearer ' + apiKey },`,
    '  body: JSON.stringify(payload),', '})', 'if (!response.ok) throw new Error(await response.text())',
    ...(template === 'speech' ? ["await writeFile('speech.mp3', Buffer.from(await response.arrayBuffer()))"] : streaming
      ? ['const decoder = new TextDecoder()', 'for await (const chunk of response.body) {', '  process.stdout.write(decoder.decode(chunk, { stream: true }))', '}', 'process.stdout.write(decoder.decode())']
      : ['console.log(await response.text())']),
  ].join('\n')
  return { url, base: normalized, body, curl, python, javascript }
}
