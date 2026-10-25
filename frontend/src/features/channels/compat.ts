import { useQuery } from '@tanstack/react-query'
import { apiFetch } from '@/lib/api'
import type { VendorInfo } from '@/features/compat/types'
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
  /// 错误阶段的触发条件（状态码 / 错误码 / 错误体片段）。
  when?: Record<string, unknown>
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

/// 内置厂商（目录项）：主机、协议→基址表（创建兼容渠道时补全地址）、声明式探针、特殊要求条数。
export type VendorOption = VendorInfo

/// 适配档案（`custom:<slug>`）：渠道的 `vendor` 也可以选它。
export interface ProfileOption {
  id: string
  slug: string
  display_name: string
  description: string | null
  /// 它叠加的内置厂商（`builtin:<id>`）。
  extends: string | null
  hosts: string[]
}

export interface VendorCatalog {
  data: VendorOption[]
  profiles: ProfileOption[]
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

/// 缺字段按「没有」处理（桩数据、旧版本接口都不该让下拉渲染崩掉）。
export function normalizeVendors(raw: Partial<VendorCatalog> | null | undefined): VendorCatalog {
  return {
    data: (raw?.data ?? []).map((v) => ({ ...v, hosts: v.hosts ?? [], endpoints: v.endpoints ?? [], probes: v.probes ?? {} })),
    profiles: (raw?.profiles ?? []).map((p) => ({ ...p, hosts: p.hosts ?? [], description: p.description ?? null, extends: p.extends ?? null })),
  }
}

export function useCompatVendors(enabled: boolean) {
  return useQuery({
    queryKey: qk.compatVendors,
    enabled,
    staleTime: 300_000,
    queryFn: async () => normalizeVendors(await apiFetch<Partial<VendorCatalog>>('/admin/compat/vendors')),
  })
}

/// 选了厂商（或叠加了厂商的档案）后，这个协议的端点表；创建兼容渠道时拿来补全 / 让管理员选地址。
/// 档案自己没有端点，取它 `extends` 的内置厂商的。
export function presetEndpoints(
  vendors: VendorCatalog | undefined,
  id: string | null | undefined,
  protocol = 'openai.chat',
): { base: string; label?: string }[] {
  if (id === null || id === undefined || vendors === undefined) return []
  const builtin = vendors.profiles.find((p) => p.id === id)?.extends ?? id
  const entry = vendors.data.find((v) => v.id === builtin)
  return (entry?.endpoints ?? []).filter((e) => e.protocol === protocol)
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
