/// OpenAI Chat Completions 流式响应的浏览器侧解析（IMPLEMENTATION §11.39）。
///
/// 不用 `EventSource`：它只支持 GET、不能带 Authorization 头。这里用 `fetch` + `ReadableStream`
/// 手切 SSE：按空行分事件、取 `data:` 行、`[DONE]` 收尾。纯函数部分（`parseSseChunk` /
/// `applyChunk`）与网络无关，便于单测与 e2e 桩。

export interface ChatUsage {
  prompt_tokens: number
  completion_tokens: number
  cached_tokens: number
  reasoning_tokens: number
}

export interface StreamDelta {
  /// 正文增量（可能为空串）。
  content: string
  /// 推理增量（reasoning_content / reasoning）；客户端可选择折叠显示。
  reasoning: string
  /// 上游实际服务的模型名（首个带 model 的块）。
  model: string | null
  usage: ChatUsage | null
  finish: string | null
}

interface RawChunk {
  model?: string
  choices?: Array<{
    delta?: { content?: string | null; reasoning_content?: string | null; reasoning?: string | null }
    finish_reason?: string | null
  }>
  usage?: {
    prompt_tokens?: number
    completion_tokens?: number
    prompt_tokens_details?: { cached_tokens?: number }
    completion_tokens_details?: { reasoning_tokens?: number }
  } | null
}

/// 一个 SSE 事件的 data 行 JSON → 增量。认不出的块返回空增量而不是抛错：
/// 上游偶发的心跳 / 未知事件不该打断一段正在显示的回复。
export function applyChunk(data: string): StreamDelta {
  const empty: StreamDelta = { content: '', reasoning: '', model: null, usage: null, finish: null }
  let raw: RawChunk
  try {
    raw = JSON.parse(data) as RawChunk
  } catch {
    return empty
  }
  const choice = raw.choices?.[0]
  const usage = raw.usage
    ? {
        prompt_tokens: raw.usage.prompt_tokens ?? 0,
        completion_tokens: raw.usage.completion_tokens ?? 0,
        cached_tokens: raw.usage.prompt_tokens_details?.cached_tokens ?? 0,
        reasoning_tokens: raw.usage.completion_tokens_details?.reasoning_tokens ?? 0,
      }
    : null
  return {
    content: choice?.delta?.content ?? '',
    reasoning: choice?.delta?.reasoning_content ?? choice?.delta?.reasoning ?? '',
    model: typeof raw.model === 'string' && raw.model !== '' ? raw.model : null,
    usage,
    finish: choice?.finish_reason ?? null,
  }
}

/// 把新到的字节切成完整 SSE 事件的 data 载荷；未闭合的尾巴留在 `carry` 里等下一块。
export function parseSseChunk(carry: string, chunk: string): { events: string[]; carry: string } {
  const text = carry + chunk
  const parts = text.split(/\r?\n\r?\n/)
  const rest = parts.pop() ?? ''
  const events: string[] = []
  for (const block of parts) {
    const data = block
      .split(/\r?\n/)
      .filter((line) => line.startsWith('data:'))
      .map((line) => line.slice(5).trimStart())
      .join('\n')
    if (data !== '') events.push(data)
  }
  return { events, carry: rest }
}

export interface ChatMessage {
  role: 'system' | 'user' | 'assistant'
  content: string
}

export interface ChatParams {
  model: string
  messages: ChatMessage[]
  temperature?: number
  top_p?: number
  max_tokens?: number
}

export interface StreamCallbacks {
  onDelta: (delta: StreamDelta) => void
  onDone: () => void
  /// `status` 为 HTTP 状态（网络 / 流中断为 0），供 `errors:http_<status>` 兜底文案。
  onError: (code: string, status: number, param?: string) => void
}

interface ErrorEnvelope {
  error?: { code?: string; type?: string; param?: string }
}

/// 发起一次流式对话。`key` 是登录 key（页内直用，不落任何文件）。返回中断函数。
/// 中继地址是同源 `/api/me/playground/chat`：数据面不开 CORS，控制面在本进程内转交处理器。
export function streamChat(params: ChatParams, key: string, cb: StreamCallbacks): () => void {
  const controller = new AbortController()
  void (async () => {
    let resp: Response
    try {
      resp = await fetch('/api/me/playground/chat', {
        method: 'POST',
        headers: { Authorization: `Bearer ${key}`, 'Content-Type': 'application/json' },
        body: JSON.stringify({ ...params, stream: true }),
        signal: controller.signal,
      })
    } catch {
      if (!controller.signal.aborted) cb.onError('network_error', 0)
      return
    }
    if (!resp.ok) {
      let code = `http_${resp.status}`
      let param: string | undefined
      try {
        const body = (await resp.json()) as ErrorEnvelope
        code = body.error?.code ?? body.error?.type ?? code
        param = body.error?.param
      } catch {
        // 非 JSON 错误体：保留 http_<status>
      }
      cb.onError(code, resp.status, param)
      return
    }
    const reader = resp.body?.getReader()
    if (!reader) {
      cb.onError('stream_unavailable', 0)
      return
    }
    const decoder = new TextDecoder()
    let carry = ''
    try {
      for (;;) {
        const { value, done } = await reader.read()
        if (done) break
        const parsed = parseSseChunk(carry, decoder.decode(value, { stream: true }))
        carry = parsed.carry
        for (const data of parsed.events) {
          if (data === '[DONE]') {
            cb.onDone()
            return
          }
          cb.onDelta(applyChunk(data))
        }
      }
      // 没有 [DONE] 也算正常收尾（Anthropic / Gemini 方言经转换后可能不发）
      cb.onDone()
    } catch {
      if (!controller.signal.aborted) cb.onError('stream_error', 0)
    }
  })()
  return () => controller.abort()
}
