/// 试用台的模型 / 系统提示词 / 采样参数记在本机（按用户隔离）：刷新或离开再回来还是上次那套。
/// 存的是输入框里的原文（含尚未填完的数字），校验仍在页面里做——存储里的值不可信。
export interface Settings {
  model: string
  system: string
  temperature: string
  topP: string
  maxTokens: string
  /// 选用的密钥 id（字符串）；'' = 用登录会话本身。
  keyId: string
  reasoningEffort: string
  thinkingBudget: string
  /// 工具定义原文（JSON 文本，可空）。
  tools: string
}

const key = (userId: number) => `okapi.playground.settings.${userId}`
// keyId 是后加的字段：旧存档里没有它，读出时补空，所以不在必填列表里
const FIELDS: Array<keyof Settings> = ['model', 'system', 'temperature', 'topP', 'maxTokens']

export function readSettings(userId: number | undefined): Settings | null {
  if (userId === undefined) return null
  try {
    const raw = JSON.parse(localStorage.getItem(key(userId)) ?? 'null') as Record<string, unknown> | null
    if (raw === null || typeof raw !== 'object' || FIELDS.some((f) => typeof raw[f] !== 'string')) return null
    return { ...(raw as unknown as Settings), keyId: typeof raw.keyId === 'string' ? raw.keyId : '',
      reasoningEffort: typeof raw.reasoningEffort === 'string' ? raw.reasoningEffort : '',
      thinkingBudget: typeof raw.thinkingBudget === 'string' ? raw.thinkingBudget : '',
      tools: typeof raw.tools === 'string' ? raw.tools : '', }
  } catch {
    return null
  }
}

export function writeSettings(userId: number, settings: Settings): void {
  try {
    localStorage.setItem(key(userId), JSON.stringify(settings))
  } catch {
    // 存储不可用：只是不记住
  }
}
