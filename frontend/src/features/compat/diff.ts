import type { CompatRule } from '@/features/compat/types'

/// 规则集之间的差异：按规则 id 对齐。用来给批准的人看「这份草稿相对线上改了什么」。

export type RuleChangeKind = 'added' | 'removed' | 'changed' | 'unchanged'

export interface RuleChange {
  id: string
  kind: RuleChangeKind
  before?: CompatRule
  after?: CompatRule
}

export interface RulesDiff {
  changes: RuleChange[]
  /// 共有的规则 id 的先后顺序变了（同层同序号的规则按先后执行，顺序也是内容）。
  reordered: boolean
  /// 有任何差异。
  differs: boolean
}

const same = (a: CompatRule, b: CompatRule) => JSON.stringify(a) === JSON.stringify(b)

export function diffRules(before: CompatRule[], after: CompatRule[]): RulesDiff {
  const beforeById = new Map(before.map((rule) => [rule.id, rule]))
  const afterIds = new Set(after.map((rule) => rule.id))
  const changes: RuleChange[] = after.map((rule) => {
    const old = beforeById.get(rule.id)
    if (old === undefined) return { id: rule.id, kind: 'added', after: rule }
    return same(old, rule)
      ? { id: rule.id, kind: 'unchanged', before: old, after: rule }
      : { id: rule.id, kind: 'changed', before: old, after: rule }
  })
  for (const rule of before) {
    if (!afterIds.has(rule.id)) changes.push({ id: rule.id, kind: 'removed', before: rule })
  }
  const common = before.filter((rule) => afterIds.has(rule.id)).map((rule) => rule.id)
  const commonAfter = after.filter((rule) => beforeById.has(rule.id)).map((rule) => rule.id)
  const reordered = common.join('\n') !== commonAfter.join('\n')
  return { changes, reordered, differs: reordered || changes.some((change) => change.kind !== 'unchanged') }
}

export type LineKind = 'same' | 'add' | 'del'

export interface DiffLine {
  kind: LineKind
  text: string
}

/// 超过这个行数就不做逐行比对（O(n·m)），整段当作删了又加。
const MAX_DIFF_LINES = 400

/// 逐行 diff（最长公共子序列）。规则的 JSON 通常几十行，足够。
export function lineDiff(before: string, after: string): DiffLine[] {
  const a = before.split('\n')
  const b = after.split('\n')
  if (a.length > MAX_DIFF_LINES || b.length > MAX_DIFF_LINES) {
    return [...a.map((text) => ({ kind: 'del' as const, text })), ...b.map((text) => ({ kind: 'add' as const, text }))]
  }
  // lcs[i][j] = a[i..] 与 b[j..] 的最长公共子序列长度
  const lcs: number[][] = Array.from({ length: a.length + 1 }, () => Array.from({ length: b.length + 1 }, () => 0))
  for (let i = a.length - 1; i >= 0; i--) {
    for (let j = b.length - 1; j >= 0; j--) {
      const row = lcs[i]
      const below = lcs[i + 1]
      if (row === undefined || below === undefined) continue
      row[j] = a[i] === b[j] ? (below[j + 1] ?? 0) + 1 : Math.max(below[j] ?? 0, row[j + 1] ?? 0)
    }
  }
  const out: DiffLine[] = []
  let i = 0
  let j = 0
  while (i < a.length && j < b.length) {
    if (a[i] === b[j]) {
      out.push({ kind: 'same', text: a[i] ?? '' })
      i++
      j++
    } else if ((lcs[i + 1]?.[j] ?? 0) >= (lcs[i]?.[j + 1] ?? 0)) {
      out.push({ kind: 'del', text: a[i] ?? '' })
      i++
    } else {
      out.push({ kind: 'add', text: b[j] ?? '' })
      j++
    }
  }
  for (; i < a.length; i++) out.push({ kind: 'del', text: a[i] ?? '' })
  for (; j < b.length; j++) out.push({ kind: 'add', text: b[j] ?? '' })
  return out
}

export const pretty = (value: unknown) => JSON.stringify(value, null, 2)
