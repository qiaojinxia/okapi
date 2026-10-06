/// 试用台的"工具定义"：用户贴一段 OpenAI `tools` 形状的 JSON 数组，随请求一起发给模型。
/// 只校验到"够让上游不报形状错"的程度——字段语义（参数 schema 对不对）交给上游判定。

export const EXAMPLE_TOOLS = JSON.stringify(
  [
    {
      type: 'function',
      function: {
        name: 'get_weather',
        description: 'Get the current weather for a city.',
        parameters: {
          type: 'object',
          properties: { city: { type: 'string', description: 'City name, e.g. Paris' } },
          required: ['city'],
        },
      },
    },
  ],
  null,
  2,
)

export type ToolsParse =
  | { tools: null; error: null }
  | { tools: unknown[]; error: null }
  | { tools: null; error: 'syntax' | 'shape' }

/// 空白 = 不带工具；语法错与形状错分开报，错误文案不一样。
export function parseTools(text: string): ToolsParse {
  if (text.trim() === '') return { tools: null, error: null }
  let value: unknown
  try {
    value = JSON.parse(text)
  } catch {
    return { tools: null, error: 'syntax' }
  }
  const valid = Array.isArray(value) && value.length > 0 && value.every(isTool)
  return valid ? { tools: value as unknown[], error: null } : { tools: null, error: 'shape' }
}

function isTool(item: unknown): boolean {
  if (typeof item !== 'object' || item === null) return false
  const { type, function: fn } = item as { type?: unknown; function?: unknown }
  if (type !== 'function' || typeof fn !== 'object' || fn === null) return false
  const name = (fn as { name?: unknown }).name
  return typeof name === 'string' && name.trim() !== ''
}

/// 参数 JSON 缩进展示；流式途中是残缺文本、或上游给了非 JSON，就原样显示。
export function prettyArguments(raw: string): { text: string; json: boolean } {
  if (raw.trim() === '') return { text: '', json: false }
  try {
    return { text: JSON.stringify(JSON.parse(raw), null, 2), json: true }
  } catch {
    return { text: raw, json: false }
  }
}
