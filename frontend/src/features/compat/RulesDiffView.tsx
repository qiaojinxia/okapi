import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { diffRules, lineDiff, pretty, type DiffLine, type RuleChange } from '@/features/compat/diff'
import type { CompatRule } from '@/features/compat/types'
import { cn } from '@/lib/utils'

const LINE_CLASS: Record<DiffLine['kind'], string> = {
  same: 'text-muted-foreground',
  add: 'bg-success/12 text-success',
  del: 'bg-destructive/10 text-destructive',
}
const LINE_MARK: Record<DiffLine['kind'], string> = { same: ' ', add: '+', del: '-' }

function Lines({ lines }: { lines: DiffLine[] }) {
  return (
    <pre className="max-h-72 overflow-auto rounded-md border border-border bg-muted/30 py-1 font-mono text-xs leading-5">
      {lines.map((line, index) => (
        <div key={index} className={cn('flex gap-2 px-2 whitespace-pre', LINE_CLASS[line.kind])}>
          <span aria-hidden className="w-3 shrink-0 select-none">{LINE_MARK[line.kind]}</span>
          <span>{line.text}</span>
        </div>
      ))}
    </pre>
  )
}

function ChangeBlock({ change }: { change: RuleChange }) {
  const { t } = useTranslation()
  const lines: DiffLine[] =
    change.kind === 'added'
      ? pretty(change.after).split('\n').map((text) => ({ kind: 'add', text }))
      : change.kind === 'removed'
        ? pretty(change.before).split('\n').map((text) => ({ kind: 'del', text }))
        : lineDiff(pretty(change.before), pretty(change.after))
  const variant = change.kind === 'added' ? 'success' : change.kind === 'removed' ? 'destructive' : 'warning'
  return (
    <li className="flex flex-col gap-1.5">
      <div className="flex flex-wrap items-center gap-2 text-xs">
        <Badge variant={variant}>{t(`compat:diff_${change.kind}`)}</Badge>
        <code className="break-all">{change.id}</code>
      </div>
      <Lines lines={lines} />
    </li>
  )
}

/// 两份规则集的差异（按规则 id 对齐，改动的规则逐行比对）。
/// 草稿对线上版本、历史版本对当前版本都用它。
export function RulesDiffView({ before, after }: { before: CompatRule[]; after: CompatRule[] }) {
  const { t } = useTranslation()
  const diff = diffRules(before, after)
  const count = (kind: RuleChange['kind']) => diff.changes.filter((c) => c.kind === kind).length
  const shown = diff.changes.filter((change) => change.kind !== 'unchanged')
  if (!diff.differs) return <p className="text-xs text-muted-foreground">{t('compat:diffNone')}</p>
  return (
    <div className="flex flex-col gap-3">
      <p className="text-xs text-muted-foreground">
        {t('compat:diffSummary', { added: count('added'), changed: count('changed'), removed: count('removed') })}
        {count('unchanged') > 0 && ` · ${t('compat:diffUnchanged', { n: count('unchanged') })}`}
      </p>
      {diff.reordered && <p role="note" className="text-xs text-warning">{t('compat:diffReordered')}</p>}
      <ul className="flex flex-col gap-3">
        {shown.map((change) => (
          <ChangeBlock key={`${change.kind}:${change.id}`} change={change} />
        ))}
      </ul>
    </div>
  )
}
