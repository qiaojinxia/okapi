import { useQuery } from '@tanstack/react-query'
import { apiFetch } from '@/lib/api'
import { qk } from '@/lib/query-keys'

/// 适配规则（docs/compat-rules.md）在前端的形状：渠道抽屉里的「剥字段 / 注入字段」两个编辑器
/// 读写的是渠道层的两条约定规则，其余规则原样保留、只读展示。

export interface CompatOp {
  op: string
  [key: string]: unknown
}

export interface CompatRule {
  id: string
  phase?: string
  select?: Record<string, unknown>
  do?: CompatOp[]
  order?: number
  disabled?: boolean
  required?: boolean
  billing_critical_ack?: boolean
  if?: string
  doc?: { why?: string; url?: string; verified_at?: string }
}

export type CompatLayer = 'vendor' | 'profile' | 'channel' | 'guard'

export interface EffectiveRule {
  layer: CompatLayer
  rule: CompatRule
}

export interface CompatView {
  vendor: string | null
  suggested_vendor: { id: string; display_name: string } | null
  channel: { version: number; rules: CompatRule[]; note: string | null } | null
  guard: { version: number } | null
  effective: {
    layers: { layer: CompatLayer; label: string; rules: number }[]
    rules: EffectiveRule[]
    shadowed: { id: string; layer: CompatLayer; by: CompatLayer; reason: 'overridden' | 'disabled' }[]
  }
}

export interface VendorOption {
  id: string
  display_name: string
  group: string
  hosts: string[]
  rules: number
}

/// 缺字段按「没有」处理：接口以后加字段、桩数据少字段都不该把整个抽屉渲染崩掉。
function normalizeView(raw: Partial<CompatView> | null | undefined): CompatView {
  return {
    vendor: raw?.vendor ?? null,
    suggested_vendor: raw?.suggested_vendor ?? null,
    channel: raw?.channel ?? null,
    guard: raw?.guard ?? null,
    effective: {
      layers: raw?.effective?.layers ?? [],
      rules: raw?.effective?.rules ?? [],
      shadowed: raw?.effective?.shadowed ?? [],
    },
  }
}

export function useChannelCompat(channelId: number | undefined) {
  return useQuery({
    queryKey: qk.channelCompat(channelId ?? 0),
    enabled: channelId !== undefined,
    queryFn: async () => normalizeView(await apiFetch<Partial<CompatView>>(`/admin/channels/${channelId}/compat`)),
  })
}

export function useCompatVendors(enabled: boolean) {
  return useQuery({
    queryKey: qk.compatVendors,
    enabled,
    staleTime: 300_000,
    queryFn: () => apiFetch<{ data: VendorOption[] }>('/admin/compat/vendors'),
  })
}

/// 两条约定规则的 id：编辑器只管它们，别的规则一概不动。
export const STRIP_RULE_ID = 'strip-request-fields'
export const INJECT_RULE_ID = 'inject-request-fields'

/// RFC 6901：键里的 `~` 与 `/` 要转义。
export function toPointer(field: string): string {
  return `/${field.replaceAll('~', '~0').replaceAll('/', '~1')}`
}

/// 只认请求顶层的单段指针（`/user`）；多段、通配或写法不合法的返回 null。
export function fromPointer(pointer: string): string | null {
  if (!pointer.startsWith('/')) return null
  const segment = pointer.slice(1)
  if (segment === '' || segment === '*' || segment.includes('/')) return null
  if (/~(?![01])/.test(segment) || segment.endsWith('~')) return null
  return segment.replaceAll('~1', '/').replaceAll('~0', '~')
}

const SIMPLE_KEYS = new Set(['id', 'do', 'billing_critical_ack', 'doc'])

function isSimple(rule: CompatRule): boolean {
  return Object.keys(rule).every((key) => SIMPLE_KEYS.has(key))
}

function readStrip(rule: CompatRule): string[] | null {
  if (!isSimple(rule) || rule.do?.length !== 1) return null
  const op = rule.do[0]
  const paths = op?.op === 'remove' && Array.isArray(op.paths) && Object.keys(op).length === 2 ? (op.paths as unknown[]) : null
  if (paths === null) return null
  const fields = paths.map((p) => (typeof p === 'string' ? fromPointer(p) : null))
  return fields.every((f): f is string => f !== null) ? fields : null
}

function readInject(rule: CompatRule): Record<string, unknown> | null {
  if (!isSimple(rule) || !rule.do?.length) return null
  const fields: Record<string, unknown> = {}
  for (const op of rule.do) {
    const keys = Object.keys(op)
    const plain = op.op === 'set' && keys.every((k) => ['op', 'path', 'value'].includes(k)) && keys.includes('value')
    const field = plain && typeof op.path === 'string' ? fromPointer(op.path) : null
    if (field === null) return null
    fields[field] = op.value
  }
  return fields
}

export interface SimpleRules {
  strip: string[]
  inject: Record<string, unknown>
  /// 约定规则里含了编辑器表达不了的写法（嵌套路径、条件、其它操作…）：该编辑器只读，规则原样保留。
  stripLocked: boolean
  injectLocked: boolean
}

export function readSimpleRules(rules: CompatRule[]): SimpleRules {
  const strip = rules.find((r) => r.id === STRIP_RULE_ID)
  const inject = rules.find((r) => r.id === INJECT_RULE_ID)
  const stripFields = strip ? readStrip(strip) : []
  const injectFields = inject ? readInject(inject) : {}
  return {
    strip: stripFields ?? [],
    inject: injectFields ?? {},
    stripLocked: stripFields === null,
    injectLocked: injectFields === null,
  }
}

/// 把两个编辑器的内容写回规则数组：约定规则原位替换（空了就删掉，原来没有就放到最前面，先剥后注），
/// 其它规则的内容与顺序不动。锁定的编辑器对应的规则原样保留。
export function writeSimpleRules(
  original: CompatRule[],
  strip: string[],
  inject: Record<string, unknown>,
): CompatRule[] {
  const locks = readSimpleRules(original)
  const oldStrip = original.find((r) => r.id === STRIP_RULE_ID)
  const oldInject = original.find((r) => r.id === INJECT_RULE_ID)
  const fields = [...new Set(strip.map((f) => f.trim()).filter((f) => f !== ''))]
  const stripRule: CompatRule | null = locks.stripLocked
    ? (oldStrip ?? null)
    : fields.length === 0
      ? null
      : {
          id: STRIP_RULE_ID,
          do: [{ op: 'remove', paths: fields.map(toPointer) }],
          // 剥 stream_options 会让流式结算落到字符估算：后端要求规则带确认，界面另有提示
          ...(fields.includes('stream_options') ? { billing_critical_ack: true } : {}),
          ...(oldStrip?.doc ? { doc: oldStrip.doc } : {}),
        }
  const entries = Object.entries(inject).filter(([name]) => name.trim() !== '')
  const injectRule: CompatRule | null = locks.injectLocked
    ? (oldInject ?? null)
    : entries.length === 0
      ? null
      : {
          id: INJECT_RULE_ID,
          do: entries.map(([name, value]) => ({ op: 'set', path: toPointer(name), value })),
          ...(entries.some(([name]) => name === 'stream_options') ? { billing_critical_ack: true } : {}),
          ...(oldInject?.doc ? { doc: oldInject.doc } : {}),
        }
  const next = original.flatMap((rule) => {
    if (rule.id === STRIP_RULE_ID) return stripRule ? [stripRule] : []
    if (rule.id === INJECT_RULE_ID) return injectRule ? [injectRule] : []
    return [rule]
  })
  // 原来没有的约定规则：先剥后注。剥放在注的前面，注放在剥的后面；两者都没有就放最前面
  if (oldStrip === undefined && stripRule) {
    const at = next.findIndex((r) => r.id === INJECT_RULE_ID)
    next.splice(at === -1 ? 0 : at, 0, stripRule)
  }
  if (oldInject === undefined && injectRule) {
    const at = next.findIndex((r) => r.id === STRIP_RULE_ID)
    next.splice(at === -1 ? 0 : at + 1, 0, injectRule)
  }
  return next
}

export function sameRules(a: CompatRule[], b: CompatRule[]): boolean {
  return JSON.stringify(a) === JSON.stringify(b)
}
