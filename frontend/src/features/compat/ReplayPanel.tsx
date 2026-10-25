import { Play } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Input, Label, Textarea } from '@/components/ui/input'
import { Segmented } from '@/components/ui/segmented'
import { Select } from '@/components/ui/select'
import { Switch } from '@/components/ui/switch'
import { useCatalog } from '@/features/compat/api'
import { lineDiff, pretty, type DiffLine } from '@/features/compat/diff'
import { IssueList } from '@/features/compat/RulesWorkbench'
import { REPLAY_PHASES, type Replay, type ReplayPhase } from '@/features/compat/replay'
import type { DryRunResult } from '@/features/compat/types'
import { describeError } from '@/lib/i18n'
import { cn } from '@/lib/utils'

const LINE_CLASS: Record<DiffLine['kind'], string> = {
  same: 'text-muted-foreground',
  add: 'bg-success/12 text-success',
  del: 'bg-destructive/10 text-destructive',
}
const LINE_MARK: Record<DiffLine['kind'], string> = { same: ' ', add: '+', del: '-' }

export function LayerBadge({ layer }: { layer: string }) {
  const { t } = useTranslation()
  const variant = layer === 'vendor' ? 'info' : layer === 'guard' ? 'warning' : layer === 'channel' ? 'default' : 'muted'
  return <Badge variant={variant}>{t(`admin:compatLayer_${layer}`, { defaultValue: layer })}</Badge>
}

function sampleOf(replay: Replay): string {
  const { form } = replay
  return form.phase === 'response' ? form.response : form.request
}

function Result({ result, replay }: { result: DryRunResult; replay: Replay }) {
  const { t } = useTranslation()
  if (!result.valid) return <IssueList issues={result.issues ?? []} />
  const phase = result.phase
  if (phase === 'capability') {
    const facts = Object.entries(result.facts ?? {})
    return (
      <div className="flex flex-col gap-2 text-xs">
        <p className="font-medium">{t('compat:capabilityFacts')}</p>
        {facts.length === 0 ? (
          <p className="text-muted-foreground">{t('compat:capabilityNone')}</p>
        ) : (
          <ul className="flex flex-wrap gap-2">
            {facts.map(([name, value]) => (
              <li key={name}><Badge variant={value ? 'success' : 'destructive'}>{name}: {String(value)}</Badge></li>
            ))}
          </ul>
        )}
        <p className="font-medium">{t('compat:capabilityMerged')}</p>
        <pre className="max-h-48 overflow-auto rounded-md border border-border bg-muted/30 p-2 font-mono leading-5">
          {pretty(result.merged ?? null)}
        </pre>
      </div>
    )
  }
  if (phase === 'error') {
    const reactions = result.reactions ?? []
    if (reactions.length === 0) return <p className="text-xs text-muted-foreground">{t('compat:errorNoMatch')}</p>
    return (
      <div className="flex flex-col gap-2 text-xs">
        <p className="text-muted-foreground">{t('compat:errorOrderHint')}</p>
        <ol className="flex flex-col gap-2">
          {reactions.map((reaction, index) => (
            <li
              key={`${reaction.rule}:${index}`}
              className={cn(
                'flex flex-col gap-1 rounded-md border px-3 py-2',
                result.chosen === index ? 'border-primary/50 bg-primary/5' : 'border-border',
              )}
            >
              <span className="flex flex-wrap items-center gap-2">
                <LayerBadge layer={reaction.layer} />
                <code className="break-all">{reaction.rule}</code>
                <Badge variant="outline">{t(`compat:action_${reaction.action}`)}</Badge>
                {result.chosen === index && <Badge variant="default">{t('compat:chosen')}</Badge>}
                {!reaction.viable && <Badge variant="warning">{t('compat:notViable')}</Badge>}
              </span>
              <pre className="overflow-auto font-mono leading-5 text-muted-foreground">{pretty(reaction.detail)}</pre>
            </li>
          ))}
        </ol>
      </div>
    )
  }
  const sample = (() => {
    try {
      return pretty(JSON.parse(sampleOf(replay)))
    } catch {
      return sampleOf(replay)
    }
  })()
  const lines = result.changed === true ? lineDiff(sample, pretty(result.after)) : []
  return (
    <div className="flex flex-col gap-3 text-xs">
      <p className="flex flex-wrap items-center gap-2">
        <Badge variant={result.changed === true ? 'warning' : 'muted'}>
          {result.changed === true ? t('compat:changed') : t('compat:unchanged')}
        </Badge>
        {(result.layers ?? []).map((layer) => (
          <span key={layer.label} className="flex items-center gap-1 text-muted-foreground">
            <LayerBadge layer={layer.layer} />
            {layer.rules}
          </span>
        ))}
      </p>
      {lines.length > 0 && (
        <pre className="max-h-80 overflow-auto rounded-md border border-border bg-muted/30 py-1 font-mono leading-5">
          {lines.map((line, index) => (
            <div key={index} className={cn('flex gap-2 px-2 whitespace-pre', LINE_CLASS[line.kind])}>
              <span aria-hidden className="w-3 shrink-0 select-none">{LINE_MARK[line.kind]}</span>
              <span>{line.text}</span>
            </div>
          ))}
        </pre>
      )}
      <div className="grid gap-3 sm:grid-cols-2">
        <section className="flex flex-col gap-1">
          <h4 className="font-medium">{t('compat:firedTitle')}</h4>
          {(result.fired ?? []).length === 0 ? (
            <p className="text-muted-foreground">{t('compat:firedNone')}</p>
          ) : (
            <ul className="flex flex-col gap-1">
              {(result.fired ?? []).map((fired) => (
                <li key={`${fired.layer}:${fired.rule}`} className="flex flex-wrap items-center gap-2">
                  <LayerBadge layer={fired.layer} />
                  <code className="break-all">{fired.rule}</code>
                  <span className="text-muted-foreground">{t('compat:firedChanges', { n: fired.changes })}</span>
                </li>
              ))}
            </ul>
          )}
        </section>
        <section className="flex flex-col gap-1">
          <h4 className="font-medium">{t('compat:skippedTitle')}</h4>
          {(result.skipped ?? []).length === 0 ? (
            <p className="text-muted-foreground">{t('compat:skippedNone')}</p>
          ) : (
            <ul className="flex flex-col gap-1">
              {(result.skipped ?? []).map((skipped) => (
                <li key={`${skipped.layer}:${skipped.rule}`} className="flex flex-wrap items-center gap-2">
                  <LayerBadge layer={skipped.layer} />
                  <code className="break-all">{skipped.rule}</code>
                  <span className="text-muted-foreground">{t(`compat:skip_${skipped.reason}`, { defaultValue: skipped.reason })}</span>
                </li>
              ))}
            </ul>
          )}
        </section>
      </div>
    </div>
  )
}

/// 试跑：在样本上执行编辑器里的规则（叠在所选厂商之上当渠道层），不落库、不发上游，
/// 与线上走同一份引擎代码。请求 / 响应给前后差异与命中轨迹，错误给按顺序命中的反应，能力给合并出的事实。
export function ReplayPanel({ replay }: { replay: Replay }) {
  const { t } = useTranslation()
  const catalog = useCatalog()
  const { form, patch } = replay
  const values = (name: string) =>
    catalog.data?.selector.fields.find((field) => field.name === name)?.values?.map((value) => ({ value, label: value })) ?? []
  const dialects = values('dialect')
  const operations = values('operation')
  const reason = replay.problem === null ? null : t(`compat:replayProblem_${replay.problem}`)

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-col gap-1.5">
        <Label>{t('compat:replayPhase')}</Label>
        <Segmented<ReplayPhase>
          ariaLabel={t('compat:replayPhase')}
          value={form.phase}
          onChange={(phase) => patch({ phase })}
          options={REPLAY_PHASES.map((phase) => ({ value: phase, label: t(`compat:phase_${phase}`) }))}
        />
        <p className="text-xs text-muted-foreground">{t(`compat:phaseHint_${form.phase}`)}</p>
      </div>

      <div className="grid gap-3 sm:grid-cols-2">
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="replay-model">{t('compat:replayModel')}</Label>
          <Input id="replay-model" value={form.model} onChange={(e) => patch({ model: e.target.value })} />
        </div>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="replay-host">{t('compat:replayHost')}</Label>
          <Input
            id="replay-host"
            value={form.host}
            placeholder="api.example.com"
            onChange={(e) => patch({ host: e.target.value })}
          />
        </div>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="replay-dialect">{t('compat:replayDialect')}</Label>
          <Select
            id="replay-dialect"
            value={form.dialect}
            options={dialects.length > 0 ? dialects : [{ value: form.dialect, label: form.dialect }]}
            onChange={(dialect) => patch({ dialect })}
          />
        </div>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="replay-operation">{t('compat:replayOperation')}</Label>
          <Select
            id="replay-operation"
            value={form.operation}
            options={operations.length > 0 ? operations : [{ value: form.operation, label: form.operation }]}
            onChange={(operation) => patch({ operation })}
          />
        </div>
      </div>
      <Switch checked={form.stream} onChange={(stream) => patch({ stream })} label={t('compat:replayStream')} />

      {form.phase === 'request' && (
        <SampleField id="replay-request" label={t('compat:sampleRequest')} value={form.request} onChange={(request) => patch({ request })} />
      )}
      {form.phase === 'response' && (
        <SampleField
          id="replay-response"
          label={t('compat:sampleResponse')}
          hint={t('compat:sampleResponseHint')}
          value={form.response}
          onChange={(response) => patch({ response })}
        />
      )}
      {form.phase === 'error' && (
        <>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="replay-status">{t('compat:sampleStatus')}</Label>
            <Input
              id="replay-status"
              inputMode="numeric"
              className="w-28"
              value={form.errorStatus}
              onChange={(e) => patch({ errorStatus: e.target.value })}
            />
          </div>
          <SampleField id="replay-error" label={t('compat:sampleErrorBody')} value={form.error} onChange={(error) => patch({ error })} />
          <SampleField
            id="replay-error-request"
            label={t('compat:sampleErrorRequest')}
            hint={t('compat:sampleErrorRequestHint')}
            value={form.request}
            onChange={(request) => patch({ request })}
          />
        </>
      )}

      <div className="flex flex-wrap items-center gap-3">
        <Button loading={replay.running} disabled={replay.problem !== null} onClick={replay.run}>
          <Play className="h-4 w-4" />
          {t('compat:replayRun')}
        </Button>
        {reason !== null && <span className="text-xs text-muted-foreground">{reason}</span>}
      </div>

      {replay.failure !== null && replay.failure !== undefined && (
        <p role="alert" className="text-xs text-destructive">{describeError(replay.failure)}</p>
      )}
      {replay.result !== undefined && (
        <section aria-label={t('compat:replayResult')} className="flex flex-col gap-2 border-t border-border pt-4">
          <h3 className="text-sm font-medium">{t('compat:replayResult')}</h3>
          <Result result={replay.result} replay={replay} />
        </section>
      )}
    </div>
  )
}

function SampleField({
  id,
  label,
  hint,
  value,
  onChange,
}: {
  id: string
  label: string
  hint?: string
  value: string
  onChange: (value: string) => void
}) {
  return (
    <div className="flex flex-col gap-1.5">
      <Label htmlFor={id}>{label}</Label>
      {hint !== undefined && <p className="text-xs text-muted-foreground">{hint}</p>}
      <Textarea
        id={id}
        value={value}
        spellCheck={false}
        rows={8}
        className="font-mono text-xs leading-5"
        onChange={(event) => onChange(event.target.value)}
      />
    </div>
  )
}
