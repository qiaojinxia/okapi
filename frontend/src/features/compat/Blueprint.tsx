import { ArrowRight } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { useCatalog } from '@/features/compat/api'
import { pretty } from '@/features/compat/diff'
import type { Replay } from '@/features/compat/replay'
import type { CatalogOp, CompatRule, Phase } from '@/features/compat/types'
import { cn } from '@/lib/utils'

/// 蓝图：把规则集画成「选择 → 条件 → 依次的操作」的流程，每条规则一行。只读视图——
/// 要改规则去「规则」页签（拖拽编辑留给后续版本）。节点的名字与字段读后端的自描述目录，
/// Rust 里新增一个操作，这里自动多一种节点。试跑之后，命中 / 跳过的规则会在节点上打标记。

const PHASE_ORDER: Phase[] = ['request_pre', 'request', 'wire', 'response', 'error', 'capability']

const MAX_VALUE = 60

const short = (value: unknown): string => {
  const text = typeof value === 'string' ? value : JSON.stringify(value)
  return text.length > MAX_VALUE ? `${text.slice(0, MAX_VALUE - 1)}…` : text
}

/// 操作里的子操作列表（each 的 `do`、retry_same 的 `mutate`）。
const isOpList = (value: unknown): value is { op: string }[] =>
  Array.isArray(value) && value.length > 0 && value.every((item) => typeof item === 'object' && item !== null && 'op' in item)

function Chips({ entries }: { entries: [string, unknown][] }) {
  return (
    <span className="flex flex-wrap gap-1">
      {entries.map(([name, value]) => (
        <span key={name} className="rounded bg-muted px-1.5 py-0.5 font-mono text-[11px] leading-4 break-all">
          {name}: {Array.isArray(value) ? value.map((v) => short(v)).join(', ') : short(value)}
        </span>
      ))}
    </span>
  )
}

function Step({ label, tone = 'plain', children }: { label: string; tone?: 'plain' | 'op'; children: React.ReactNode }) {
  return (
    <div
      className={cn(
        'flex min-w-0 max-w-full flex-col gap-1 rounded-md border px-2.5 py-1.5 text-xs',
        tone === 'op' ? 'border-primary/30 bg-primary/5' : 'border-border bg-card',
      )}
    >
      <span className="font-medium text-muted-foreground">{label}</span>
      <div className="min-w-0 break-words">{children}</div>
    </div>
  )
}

function OpNode({ op, ops, index }: { op: { op: string; [key: string]: unknown }; ops: Map<string, CatalogOp>; index: number }) {
  const { t } = useTranslation()
  const fields = ops.get(op.op)?.fields ?? []
  const entries = Object.entries(op).filter(([name]) => name !== 'op')
  const nested = entries.filter(([, value]) => isOpList(value))
  const plain = entries.filter(([, value]) => !isOpList(value))
  const known = new Set(fields.map((field) => field.name))
  return (
    <Step label={`${index + 1}. ${t(`compat:op_${op.op}`, { defaultValue: op.op })}`} tone="op">
      {plain.length > 0 && <Chips entries={plain.map(([name, value]) => [known.has(name) ? name : `${name}?`, value])} />}
      {nested.map(([name, value]) => (
        <div key={name} className="mt-1.5 flex flex-col gap-1.5 border-l-2 border-primary/30 pl-2">
          <span className="text-muted-foreground">{name}</span>
          {(value as { op: string; [key: string]: unknown }[]).map((child, childIndex) => (
            <OpNode key={childIndex} op={child} ops={ops} index={childIndex} />
          ))}
        </div>
      ))}
    </Step>
  )
}

type Mark =
  | { kind: 'fired'; changes: number }
  | { kind: 'skipped'; reason: string }
  | { kind: 'reaction'; chosen: boolean; viable: boolean }
  | { kind: 'idle' }

/// 最近一次试跑在这条规则上留下的痕迹；试跑的是别的阶段或还没试跑就没有。
/// 试跑把编辑器里的规则当渠道层，所以只认渠道层的命中。
function markOf(rule: CompatRule, replay: Replay): Mark | null {
  const result = replay.result
  const phase = rule.phase ?? 'request'
  if (result === undefined || !result.valid || result.phase !== phase || phase === 'capability') return null
  if (phase === 'error') {
    const index = (result.reactions ?? []).findIndex((r) => r.rule === rule.id && r.layer === 'channel')
    const reaction = result.reactions?.[index]
    return index >= 0 && reaction !== undefined
      ? { kind: 'reaction', chosen: result.chosen === index, viable: reaction.viable }
      : { kind: 'idle' }
  }
  const fired = (result.fired ?? []).find((f) => f.rule === rule.id && f.layer === 'channel')
  if (fired !== undefined) return { kind: 'fired', changes: fired.changes }
  const skipped = (result.skipped ?? []).find((s) => s.rule === rule.id && s.layer === 'channel')
  return skipped === undefined ? { kind: 'idle' } : { kind: 'skipped', reason: skipped.reason }
}

function MarkBadge({ mark }: { mark: Mark }) {
  const { t } = useTranslation()
  switch (mark.kind) {
    case 'fired':
      return <Badge variant="success" dot>{t('compat:markFired', { n: mark.changes })}</Badge>
    case 'skipped':
      return (
        <Badge variant="warning" dot>
          {t('compat:markSkipped', { reason: t(`compat:skip_${mark.reason}`, { defaultValue: mark.reason }) })}
        </Badge>
      )
    case 'reaction':
      return (
        <Badge variant={mark.chosen ? 'success' : 'muted'} dot>
          {mark.chosen ? t('compat:markChosen') : mark.viable ? t('compat:markMatched') : t('compat:notViable')}
        </Badge>
      )
    case 'idle':
      return <Badge variant="muted">{t('compat:markIdle')}</Badge>
  }
}

function RuleRow({
  rule,
  ops,
  mark,
  selected,
  onSelect,
}: {
  rule: CompatRule
  ops: Map<string, CatalogOp>
  mark: Mark | null
  selected: boolean
  onSelect: () => void
}) {
  const { t } = useTranslation()
  const select = Object.entries(rule.select ?? {})
  const when = Object.entries(rule.when ?? {})
  return (
    <li>
      <div
        className={cn(
          'flex flex-col gap-2 rounded-lg border bg-card px-3 py-2.5 transition-colors',
          selected ? 'border-primary/60 shadow-xs' : 'border-border',
          rule.disabled === true && 'opacity-60',
        )}
      >
        <button
          type="button"
          aria-pressed={selected}
          onClick={onSelect}
          className="flex flex-wrap items-center gap-2 text-left outline-none focus-visible:ring-2 focus-visible:ring-primary/40"
        >
          <code className="text-sm font-medium break-all">{rule.id}</code>
          {rule.disabled === true && <Badge variant="muted">{t('compat:ruleDisabled')}</Badge>}
          {rule.required === true && <Badge variant="warning">{t('compat:ruleRequired')}</Badge>}
          {rule.billing_critical_ack === true && <Badge variant="destructive">{t('compat:ruleBilling')}</Badge>}
          {rule.order !== undefined && <Badge variant="outline">{t('compat:ruleOrder', { n: rule.order })}</Badge>}
          {mark !== null && <MarkBadge mark={mark} />}
        </button>
        {rule.doc?.why !== undefined && <p className="text-xs leading-5 text-muted-foreground">{rule.doc.why}</p>}
        <div className="flex flex-wrap items-stretch gap-2">
          <Step label={t('compat:stepSelect')}>
            {select.length === 0 ? <span className="text-muted-foreground">{t('compat:selectAll')}</span> : <Chips entries={select} />}
          </Step>
          {when.length > 0 && (
            <>
              <ArrowRight aria-hidden className="mt-4 h-3.5 w-3.5 shrink-0 text-muted-foreground" />
              <Step label={t('compat:stepWhen')}>
                <Chips entries={when} />
              </Step>
            </>
          )}
          {(rule.do ?? []).map((op, index) => (
            <span key={index} className="flex min-w-0 max-w-full items-stretch gap-2">
              <ArrowRight aria-hidden className="mt-4 h-3.5 w-3.5 shrink-0 text-muted-foreground" />
              <OpNode op={op} ops={ops} index={index} />
            </span>
          ))}
        </div>
      </div>
    </li>
  )
}

export function Blueprint({ rules, replay }: { rules: CompatRule[] | null; replay: Replay }) {
  const { t } = useTranslation()
  const catalog = useCatalog()
  const [picked, setPicked] = useState<string | null>(null)
  if (rules === null) return <p role="alert" className="text-xs text-destructive">{t('compat:blueprintInvalid')}</p>
  const ops = new Map((catalog.data?.ops ?? []).map((op) => [op.name, op]))
  const groups = PHASE_ORDER.map((phase) => ({
    phase,
    // 同阶段按 order 再按书写顺序执行
    rules: rules
      .map((rule, index) => ({ rule, index }))
      .filter(({ rule }) => (rule.phase ?? 'request') === phase)
      .sort((a, b) => (a.rule.order ?? 0) - (b.rule.order ?? 0) || a.index - b.index)
      .map(({ rule }) => rule),
  })).filter((group) => group.rules.length > 0)
  const chosen = rules.find((rule) => rule.id === picked) ?? rules[0]
  if (rules.length === 0) return <p className="text-xs text-muted-foreground">{t('compat:blueprintEmpty')}</p>
  return (
    <div className="grid gap-4 lg:grid-cols-[minmax(0,1fr)_20rem]">
      <div className="flex min-w-0 flex-col gap-4">
        <p className="text-xs leading-5 text-muted-foreground">{t('compat:blueprintHint')}</p>
        {groups.map((group) => (
          <section key={group.phase} className="flex flex-col gap-2">
            <h3 className="flex items-center gap-2 text-sm font-medium">
              {t(`compat:phase_${group.phase}`, { defaultValue: group.phase })}
              <Badge variant="muted">{group.rules.length}</Badge>
            </h3>
            <ol className="flex flex-col gap-2">
              {group.rules.map((rule) => (
                <RuleRow
                  key={rule.id}
                  rule={rule}
                  ops={ops}
                  mark={markOf(rule, replay)}
                  selected={rule.id === chosen?.id}
                  onSelect={() => setPicked(rule.id)}
                />
              ))}
            </ol>
          </section>
        ))}
      </div>
      <aside aria-label={t('compat:ruleJson')} className="flex min-w-0 flex-col gap-1.5">
        <h3 className="text-sm font-medium">{t('compat:ruleJson')}</h3>
        <pre className="max-h-[32rem] overflow-auto rounded-md border border-border bg-muted/30 p-2 font-mono text-xs leading-5">
          {chosen === undefined ? '' : pretty(chosen)}
        </pre>
      </aside>
    </div>
  )
}
