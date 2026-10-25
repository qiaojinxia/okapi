import { keepPreviousData, useQuery } from '@tanstack/react-query'
import { useState } from 'react'
import { pretty } from '@/features/compat/diff'
import type { CompatRule, DryRunResult, RuleIssue, SetOverview } from '@/features/compat/types'
import { useDebounced } from '@/hooks/use-debounced'
import { apiFetch } from '@/lib/api'

/// 规则编辑器的状态：一段 JSON 文本 + 它与「当前基线」（草稿优先，其次线上版本）的差异 + 实时校验。
/// 提到抽屉层是因为蓝图与试跑页签要读同一份「正在编辑的规则」。

export interface Validation {
  /// 输入刚变过、校验还没回来。
  pending: boolean
  /// 后端的校验结论；JSON 都解析不了、或还没问过时为 null。
  valid: boolean | null
  issues: RuleIssue[]
}

export interface RulesEditor {
  text: string
  setText: (text: string) => void
  /// 文本解析成功且是数组时的规则；否则 null。
  rules: CompatRule[] | null
  /// 与基线不同（JSON 都不合法也算）。
  dirty: boolean
  baseline: CompatRule[]
  reset: () => void
  /// 把一条规则追加到末尾（id 冲突时自动加后缀）。文本不是合法数组时什么也不做，返回 false。
  insertRule: (rule: CompatRule) => boolean
  validation: Validation
}

export function parseRules(text: string): CompatRule[] | null {
  try {
    const value: unknown = JSON.parse(text)
    return Array.isArray(value) ? (value as CompatRule[]) : null
  } catch {
    return null
  }
}

/// 在已有 id 之外为配方起一个不冲突的 id：`name`、`name-2`、`name-3`…
export function freshId(taken: Iterable<string>, wanted: string): string {
  const used = new Set(taken)
  if (!used.has(wanted)) return wanted
  for (let n = 2; ; n++) {
    const candidate = `${wanted}-${n}`
    if (!used.has(candidate)) return candidate
  }
}

const baselineOf = (overview: SetOverview | undefined): CompatRule[] =>
  overview?.draft?.rules ?? overview?.published?.rules ?? []

/// 基线换了（保存 / 发布 / 别人改了）的标识；版本号加草稿的时间足够区分「同一版草稿被覆盖」。
const baselineKey = (overview: SetOverview | undefined): string =>
  overview === undefined
    ? ''
    : `${overview.draft?.version ?? '-'}:${overview.draft?.created_at ?? ''}:${overview.published?.version ?? '-'}`

const sameJson = (a: unknown, b: unknown) => JSON.stringify(a) === JSON.stringify(b)

/// 校验只要后端认不认，不要试跑结果：用能力阶段，它不会因为样本是空的而让 `required` 规则失败。
const VALIDATE_CONTEXT = { model: '', dialect: 'openai.chat', operation: 'chat', stream: false }

export function useRulesEditor(overview: SetOverview | undefined): RulesEditor {
  const baseline = baselineOf(overview)
  const key = baselineKey(overview)
  const [state, setState] = useState({ key: '', baseline: '[]', text: '[]', touched: false })

  // 基线换了：用户没动过、或编辑内容仍等于旧基线 → 跟上新基线；有未保存的修改 → 保留，只记下新基线。
  // 在渲染期对齐（同 useDraft），不走 useEffect，免得先按旧文本画一帧。
  if (overview !== undefined && state.key !== key) {
    const current = parseRules(state.text)
    const adopt = !state.touched || (current !== null && sameJson(current, JSON.parse(state.baseline)))
    setState({
      key,
      baseline: JSON.stringify(baseline),
      text: adopt ? pretty(baseline) : state.text,
      touched: adopt ? false : state.touched,
    })
  }

  const rules = parseRules(state.text)
  const dirty = rules === null || !sameJson(rules, baseline)

  const rulesKey = rules === null ? null : JSON.stringify(rules)
  const settled = useDebounced(rulesKey, 400)
  const checked = useQuery({
    queryKey: ['admin', 'compat-validate', settled] as const,
    enabled: overview !== undefined && settled !== null,
    staleTime: Number.POSITIVE_INFINITY,
    placeholderData: keepPreviousData,
    queryFn: () =>
      apiFetch<DryRunResult>('/admin/compat/dry-run', {
        method: 'POST',
        body: { rules: JSON.parse(settled ?? '[]'), phase: 'capability', context: VALIDATE_CONTEXT },
      }),
  })
  const validation: Validation =
    rulesKey === null
      ? { pending: false, valid: null, issues: [] }
      : {
          pending: rulesKey !== settled || checked.isFetching,
          valid: checked.data === undefined ? null : checked.data.valid,
          issues: checked.data?.issues ?? [],
        }

  return {
    text: state.text,
    setText: (text) => setState((s) => ({ ...s, text, touched: true })),
    rules,
    dirty,
    baseline,
    reset: () => setState((s) => ({ ...s, text: pretty(baseline), touched: false })),
    insertRule: (rule) => {
      if (rules === null) return false
      const id = freshId(
        rules.map((r) => r.id),
        rule.id,
      )
      setState((s) => ({ ...s, text: pretty([...rules, { ...rule, id }]), touched: true }))
      return true
    },
    validation,
  }
}
