import { Plus, Trash2 } from 'lucide-react'
import { useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Input, Label } from '@/components/ui/input'
import { Select } from '@/components/ui/select'
import { Switch } from '@/components/ui/switch'
import {
  OVERRIDE_MATCHERS,
  OVERRIDE_OPS,
  type OverrideCondition,
  type OverrideMatcher,
  type OverrideOp,
  type RequestOverrideRule,
} from '@/features/channels/types'

interface ConditionDraft {
  id: number
  field: string
  matcher: OverrideMatcher
  valueText: string
}

interface RuleDraft {
  id: number
  name: string
  enabled: boolean
  op: OverrideOp
  path: string
  from: string
  pattern: string
  valueText: string
  statusText: string
  conditions: ConditionDraft[]
}

function parseJsonOrText(raw: string): unknown {
  const trimmed = raw.trim()
  if (trimmed === '') return ''
  try {
    return JSON.parse(trimmed) as unknown
  } catch {
    return raw
  }
}

function formatJsonOrText(value: unknown): string {
  if (value === undefined || value === null) return ''
  if (typeof value === 'string') return value
  try {
    return JSON.stringify(value)
  } catch {
    return String(value)
  }
}

function toDraft(rule: RequestOverrideRule, nextId: () => number): RuleDraft {
  return {
    id: nextId(),
    name: rule.name ?? '',
    enabled: rule.enabled ?? true,
    op: rule.op,
    path: rule.path ?? '',
    from: rule.from ?? '',
    pattern: rule.pattern ?? '',
    valueText: formatJsonOrText(rule.value),
    statusText: rule.status !== undefined ? String(rule.status) : '400',
    conditions: (rule.conditions ?? []).map((c) => ({
      id: nextId(),
      field: c.field,
      matcher: c.matcher,
      valueText: formatJsonOrText(c.value),
    })),
  }
}

function toWire(draft: RuleDraft): RequestOverrideRule {
  const conditions: OverrideCondition[] = draft.conditions
    .filter((c) => c.field.trim() !== '')
    .map((c) => ({
      field: c.field.trim(),
      matcher: c.matcher,
      ...(c.matcher === 'exists' || c.matcher === 'missing'
        ? {}
        : {
            value:
              c.matcher === 'regex' ||
              c.matcher === 'prefix' ||
              c.matcher === 'suffix' ||
              c.matcher === 'contains'
                ? c.valueText
                : parseJsonOrText(c.valueText),
          }),
    }))

  const rule: RequestOverrideRule = {
    enabled: draft.enabled,
    op: draft.op,
    ...(draft.name.trim() !== '' ? { name: draft.name.trim() } : {}),
    ...(conditions.length > 0 ? { conditions } : {}),
  }

  switch (draft.op) {
    case 'set':
      rule.path = draft.path.trim()
      rule.value = parseJsonOrText(draft.valueText)
      break
    case 'delete':
    case 'delete_header':
      rule.path = draft.path.trim()
      break
    case 'move':
    case 'copy':
      rule.from = draft.from.trim()
      rule.path = draft.path.trim()
      break
    case 'replace':
    case 'regex_replace':
      rule.path = draft.path.trim()
      rule.pattern = draft.pattern
      rule.value = draft.valueText
      break
    case 'set_header':
      rule.path = draft.path.trim()
      rule.value = draft.valueText
      break
    case 'return_error': {
      const code = Number(draft.statusText.trim())
      rule.status = Number.isInteger(code) && code >= 400 && code <= 599 ? code : 400
      if (draft.valueText.trim() !== '') {
        rule.value = draft.valueText.trim()
      }
      break
    }
  }
  return rule
}

const PRESETS: Record<'temp' | 'header' | 'block', RequestOverrideRule> = {
  temp: {
    name: 'Cap temperature',
    enabled: true,
    conditions: [
      { field: 'model', matcher: 'prefix', value: 'gpt-5' },
      { field: 'body:temperature', matcher: 'gt', value: 0.7 },
    ],
    op: 'set',
    path: 'temperature',
    value: 0.7,
  },
  header: {
    name: 'Conditional route header',
    enabled: true,
    conditions: [{ field: 'ingress', matcher: 'eq', value: 'openai' }],
    op: 'set_header',
    path: 'X-Custom-Route',
    value: 'standard',
  },
  block: {
    name: 'Block unwanted client',
    enabled: true,
    conditions: [{ field: 'user_agent', matcher: 'contains', value: 'LegacyBot' }],
    op: 'return_error',
    status: 403,
    value: 'client_blocked',
  },
}

/// 声明式参数 / Header / 合规拦截条件改写规则编辑器（`settings.request_overrides`）。
export function RequestOverridesEditor({
  value,
  onChange,
}: {
  value: RequestOverrideRule[]
  onChange: (next: RequestOverrideRule[]) => void
}) {
  const { t } = useTranslation()
  const seq = useRef(0)
  const nextId = () => seq.current++
  const [drafts, setDrafts] = useState<RuleDraft[]>(() => value.map((r) => toDraft(r, nextId)))

  const commit = (next: RuleDraft[]) => {
    setDrafts(next)
    onChange(next.map(toWire))
  }

  const updateRule = (id: number, patch: Partial<RuleDraft>) =>
    commit(drafts.map((r) => (r.id === id ? { ...r, ...patch } : r)))

  const removeRule = (id: number) => commit(drafts.filter((r) => r.id !== id))

  const addRule = (seed?: RequestOverrideRule) => {
    const item = toDraft(
      seed ?? { enabled: true, op: 'set', path: '', value: '' },
      nextId,
    )
    commit([...drafts, item])
  }

  const addCondition = (ruleId: number) =>
    commit(
      drafts.map((r) =>
        r.id === ruleId
          ? {
              ...r,
              conditions: [
                ...r.conditions,
                { id: nextId(), field: 'model', matcher: 'eq', valueText: '' },
              ],
            }
          : r,
      ),
    )

  const updateCondition = (ruleId: number, condId: number, patch: Partial<ConditionDraft>) =>
    commit(
      drafts.map((r) =>
        r.id === ruleId
          ? {
              ...r,
              conditions: r.conditions.map((c) => (c.id === condId ? { ...c, ...patch } : c)),
            }
          : r,
      ),
    )

  const removeCondition = (ruleId: number, condId: number) =>
    commit(
      drafts.map((r) =>
        r.id === ruleId
          ? { ...r, conditions: r.conditions.filter((c) => c.id !== condId) }
          : r,
      ),
    )

  return (
    <div data-slot="request-overrides-editor" className="flex flex-col gap-3 border-t border-border/60 pt-4">
      <div className="flex flex-col gap-1">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <Label className="text-[13px] text-foreground">{t('admin:requestOverrides')}</Label>
          {drafts.length > 0 && (
            <Badge variant="muted" className="tabular-nums">
              {drafts.length}
            </Badge>
          )}
        </div>
        <p className="text-xs leading-5 text-muted-foreground">{t('admin:requestOverridesHint')}</p>
      </div>

      {drafts.length === 0 ? (
        <p className="rounded-xl border border-dashed border-border/80 bg-muted/25 px-3.5 py-3 text-xs leading-5 text-muted-foreground">
          {t('admin:requestOverridesEmpty')}
        </p>
      ) : (
        <div className="flex flex-col gap-3">
          {drafts.map((rule, idx) => {
            const needsPath = rule.op !== 'return_error'
            const isHeaderOp = rule.op === 'set_header' || rule.op === 'delete_header'
            const needsFrom = rule.op === 'move' || rule.op === 'copy'
            const needsPattern = rule.op === 'replace' || rule.op === 'regex_replace'
            const needsValue =
              rule.op === 'set' ||
              rule.op === 'set_header' ||
              rule.op === 'replace' ||
              rule.op === 'regex_replace'
            return (
              <div
                key={rule.id}
                data-slot="override-rule-card"
                className="flex flex-col gap-3 rounded-xl border border-border/75 bg-card/75 p-3 shadow-xs"
              >
                {/* 规则头：序号 + 可选名称 + 启用开关 + 删除 */}
                <div className="flex flex-wrap items-center gap-2">
                  <span className="inline-flex h-5 min-w-5 shrink-0 items-center justify-center rounded-md bg-muted px-1.5 font-mono text-[11px] font-semibold text-muted-foreground tabular-nums">
                    #{idx + 1}
                  </span>
                  <Input
                    className="h-8 min-w-36 flex-1 text-xs"
                    value={rule.name}
                    placeholder={t('admin:overrideRuleName')}
                    aria-label={t('admin:overrideRuleName')}
                    onChange={(e) => updateRule(rule.id, { name: e.target.value })}
                  />
                  <Switch
                    hideLabel
                    className="[&>span:last-child]:mt-0"
                    label={t('admin:overrideRuleEnabled')}
                    checked={rule.enabled}
                    onChange={(enabled) => updateRule(rule.id, { enabled })}
                  />
                  <Button
                    type="button"
                    size="icon"
                    variant="ghost"
                    className="h-8 w-8 shrink-0 text-muted-foreground hover:text-destructive"
                    aria-label={t('common:delete')}
                    onClick={() => removeRule(rule.id)}
                  >
                    <Trash2 className="h-3.5 w-3.5" />
                  </Button>
                </div>

                {/* 动作 + 目标参数 */}
                <div className="grid grid-cols-1 gap-2 sm:grid-cols-2">
                  <div className="flex flex-col gap-1">
                    <Label className="text-[11px] text-muted-foreground">{t('admin:overrideOp')}</Label>
                    <Select
                      size="sm"
                      aria-label={t('admin:overrideOp')}
                      value={rule.op}
                      onChange={(v) => updateRule(rule.id, { op: v as OverrideOp })}
                      options={OVERRIDE_OPS.map((op) => ({
                        value: op,
                        label: t(`admin:overrideOp_${op}`),
                      }))}
                    />
                  </div>

                  {needsFrom && (
                    <div className="flex flex-col gap-1">
                      <Label className="text-[11px] text-muted-foreground">{t('admin:overrideFrom')}</Label>
                      <Input
                        className="h-8 font-mono text-xs"
                        value={rule.from}
                        placeholder="max_tokens"
                        aria-label={t('admin:overrideFrom')}
                        onChange={(e) => updateRule(rule.id, { from: e.target.value })}
                      />
                    </div>
                  )}

                  {needsPath && (
                    <div className="flex flex-col gap-1">
                      <Label className="text-[11px] text-muted-foreground">
                        {isHeaderOp ? t('admin:overrideHeaderName') : t('admin:overridePath')}
                      </Label>
                      <Input
                        className="h-8 font-mono text-xs"
                        value={rule.path}
                        placeholder={isHeaderOp ? 'X-Custom-Route' : 'reasoning.effort'}
                        aria-label={isHeaderOp ? t('admin:overrideHeaderName') : t('admin:overridePath')}
                        onChange={(e) => updateRule(rule.id, { path: e.target.value })}
                      />
                    </div>
                  )}

                  {needsPattern && (
                    <div className="flex flex-col gap-1">
                      <Label className="text-[11px] text-muted-foreground">{t('admin:overridePattern')}</Label>
                      <Input
                        className="h-8 font-mono text-xs"
                        value={rule.pattern}
                        placeholder={rule.op === 'regex_replace' ? '^usr_(\\d+)$' : 'old-text'}
                        aria-label={t('admin:overridePattern')}
                        onChange={(e) => updateRule(rule.id, { pattern: e.target.value })}
                      />
                    </div>
                  )}

                  {needsValue && (
                    <div className="flex flex-col gap-1">
                      <Label className="text-[11px] text-muted-foreground">
                        {isHeaderOp
                          ? t('admin:overrideHeaderValue')
                          : needsPattern
                            ? t('admin:overrideReplaceWith')
                            : t('admin:overrideValue')}
                      </Label>
                      <Input
                        className="h-8 font-mono text-xs"
                        value={rule.valueText}
                        placeholder={isHeaderOp ? 'gold' : needsPattern ? 'id-$1' : '0.7 or "medium"'}
                        aria-label={
                          isHeaderOp
                            ? t('admin:overrideHeaderValue')
                            : needsPattern
                              ? t('admin:overrideReplaceWith')
                              : t('admin:overrideValue')
                        }
                        onChange={(e) => updateRule(rule.id, { valueText: e.target.value })}
                      />
                    </div>
                  )}

                  {rule.op === 'return_error' && (
                    <>
                      <div className="flex flex-col gap-1">
                        <Label className="text-[11px] text-muted-foreground">{t('admin:overrideErrorStatus')}</Label>
                        <Input
                          className="h-8 font-mono text-xs"
                          inputMode="numeric"
                          value={rule.statusText}
                          placeholder="403"
                          aria-label={t('admin:overrideErrorStatus')}
                          onChange={(e) => updateRule(rule.id, { statusText: e.target.value })}
                        />
                      </div>
                      <div className="flex flex-col gap-1 sm:col-span-2">
                        <Label className="text-[11px] text-muted-foreground">{t('admin:overrideErrorReason')}</Label>
                        <Input
                          className="h-8 font-mono text-xs"
                          value={rule.valueText}
                          placeholder="client_blocked"
                          aria-label={t('admin:overrideErrorReason')}
                          onChange={(e) => updateRule(rule.id, { valueText: e.target.value })}
                        />
                      </div>
                    </>
                  )}
                </div>

                {/* 条件区 */}
                <div className="flex flex-col gap-2 border-t border-border/50 pt-2.5">
                  <div className="flex flex-wrap items-center justify-between gap-2">
                    <span className="text-[11px] font-medium text-muted-foreground">
                      {t('admin:overrideConditions')}
                    </span>
                    <Button
                      type="button"
                      size="sm"
                      variant="ghost"
                      className="h-7 px-2 text-xs"
                      onClick={() => addCondition(rule.id)}
                    >
                      <Plus className="h-3 w-3" />
                      {t('admin:overrideConditionAdd')}
                    </Button>
                  </div>
                  {rule.conditions.map((cond) => {
                    const needsCondVal = cond.matcher !== 'exists' && cond.matcher !== 'missing'
                    return (
                      <div
                        key={cond.id}
                        className="flex flex-wrap items-center gap-1.5 rounded-lg border border-border/60 bg-muted/25 p-1.5"
                      >
                        <Input
                          className="h-7 min-w-28 flex-1 font-mono text-xs"
                          value={cond.field}
                          placeholder="model / user_agent / body:temperature"
                          aria-label={t('admin:overrideCondField')}
                          onChange={(e) => updateCondition(rule.id, cond.id, { field: e.target.value })}
                        />
                        <Select
                          size="sm"
                          className="w-28 shrink-0"
                          aria-label={t('admin:overrideCondMatcher')}
                          value={cond.matcher}
                          onChange={(v) =>
                            updateCondition(rule.id, cond.id, { matcher: v as OverrideMatcher })
                          }
                          options={OVERRIDE_MATCHERS.map((m) => ({
                            value: m,
                            label: t(`admin:overrideMatcher_${m}`),
                          }))}
                        />
                        {needsCondVal && (
                          <Input
                            className="h-7 min-w-24 flex-1 font-mono text-xs"
                            value={cond.valueText}
                            placeholder="gpt-5"
                            aria-label={t('admin:overrideCondValue')}
                            onChange={(e) =>
                              updateCondition(rule.id, cond.id, { valueText: e.target.value })
                            }
                          />
                        )}
                        <Button
                          type="button"
                          size="icon"
                          variant="ghost"
                          className="h-7 w-7 shrink-0 text-muted-foreground hover:text-destructive"
                          aria-label={t('common:delete')}
                          onClick={() => removeCondition(rule.id, cond.id)}
                        >
                          ×
                        </Button>
                      </div>
                    )
                  })}
                </div>
              </div>
            )
          })}
        </div>
      )}

      <div className="flex flex-wrap items-center gap-2">
        <Button type="button" size="sm" variant="outline" onClick={() => addRule()}>
          <Plus className="h-3.5 w-3.5" />
          {t('admin:requestOverridesAdd')}
        </Button>
        <Button type="button" size="sm" variant="ghost" className="text-xs text-muted-foreground" onClick={() => addRule(PRESETS.temp)}>
          + {t('admin:requestOverridesPresetTemp')}
        </Button>
        <Button type="button" size="sm" variant="ghost" className="text-xs text-muted-foreground" onClick={() => addRule(PRESETS.header)}>
          + {t('admin:requestOverridesPresetHeader')}
        </Button>
        <Button type="button" size="sm" variant="ghost" className="text-xs text-muted-foreground" onClick={() => addRule(PRESETS.block)}>
          + {t('admin:requestOverridesPresetBlock')}
        </Button>
      </div>
    </div>
  )
}
