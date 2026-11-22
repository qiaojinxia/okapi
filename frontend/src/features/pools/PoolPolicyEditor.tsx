import { Plus, Trash2 } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { FieldGroup } from '@/components/ui/drawer'
import { IconButton } from '@/components/ui/icon-button'
import { Input, Label } from '@/components/ui/input'
import { Select } from '@/components/ui/select'
import { Switch } from '@/components/ui/switch'
import { TagInput } from '@/components/ui/tag-input'
import {
  AFFINITY_LABEL,
  AFFINITY_MODES,
  COMPACTION_LABEL,
  COMPACTION_MODES,
  ROUTING_STRATEGIES,
  STRATEGY_LABEL,
} from '@/features/pools/types'
import type { AffinityMode, CompactionMode, PoolPolicy, RoutingRule } from '@/features/pools/types'

function parseOptInt(raw: string): number | null {
  const t = raw.trim()
  if (t === '') return null
  const n = Number(t)
  return Number.isInteger(n) ? n : null
}

/// 调度池策略编辑器：选路与灰度 / 粘性与容灾 / 硬护栏 / 条件覆写规则。
export function PoolPolicyEditor({
  policy,
  onChange,
  poolCodes,
}: {
  policy: PoolPolicy
  onChange: (next: PoolPolicy) => void
  poolCodes: string[]
}) {
  const { t } = useTranslation()
  const rules = policy.rules ?? []

  const updateRule = (index: number, patch: RoutingRule) => {
    const next = [...rules]
    next[index] = patch
    onChange({ ...policy, rules: next })
  }

  const removeRule = (index: number) => {
    onChange({ ...policy, rules: rules.filter((_, i) => i !== index) })
  }

  const addRule = () => {
    onChange({
      ...policy,
      rules: [
        ...rules,
        {
          name: `rule-${rules.length + 1}`,
          when: {},
          apply: {},
        },
      ],
    })
  }

  return (
    <div className="flex flex-col gap-5">
      <FieldGroup title={t('admin:poolSelectionGroup')} hint={t('admin:poolSelectionGroupHint')}>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="policy-canary">{t('admin:poolCanaryPercent')}</Label>
          <Input
            id="policy-canary"
            type="number"
            min={0}
            max={100}
            className="w-32 font-mono text-sm"
            value={String(policy.canary_percent ?? 0)}
            onChange={(e) => {
              const v = parseOptInt(e.target.value)
              onChange({ ...policy, canary_percent: v === null ? 0 : Math.max(0, Math.min(100, v)) })
            }}
          />
          <p className="text-xs text-muted-foreground">{t('admin:poolCanaryHint')}</p>
        </div>

        <Switch
          checked={policy.selection?.respect_priority_layers !== false}
          onChange={(checked) =>
            onChange({
              ...policy,
              selection: { ...policy.selection, respect_priority_layers: checked },
            })
          }
          label={t('admin:poolRespectPriority')}
          description={t('admin:poolRespectPriorityDesc')}
        />

        <Switch
          checked={policy.selection?.ignore_cost_in_weight === true}
          onChange={(checked) =>
            onChange({
              ...policy,
              selection: { ...policy.selection, ignore_cost_in_weight: checked },
            })
          }
          label={t('admin:poolIgnoreCost')}
          description={t('admin:poolIgnoreCostDesc')}
        />
      </FieldGroup>

      <FieldGroup title={t('admin:poolUrgencyGroup')} hint={t('admin:poolUrgencyHint')}>
        <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="policy-urg-short">{t('admin:poolUrgencyShortWeight')}</Label>
            <Input
              id="policy-urg-short"
              type="number"
              min={0}
              max={10000}
              placeholder="1000"
              value={policy.selection?.urgency?.short_window_weight ?? ''}
              onChange={(e) =>
                onChange({
                  ...policy,
                  selection: {
                    ...policy.selection,
                    urgency: {
                      ...policy.selection?.urgency,
                      short_window_weight: parseOptInt(e.target.value),
                    },
                  },
                })
              }
            />
          </div>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="policy-urg-long">{t('admin:poolUrgencyLongWeight')}</Label>
            <Input
              id="policy-urg-long"
              type="number"
              min={0}
              max={20000}
              placeholder="3500"
              value={policy.selection?.urgency?.long_window_weight ?? ''}
              onChange={(e) =>
                onChange({
                  ...policy,
                  selection: {
                    ...policy.selection,
                    urgency: {
                      ...policy.selection?.urgency,
                      long_window_weight: parseOptInt(e.target.value),
                    },
                  },
                })
              }
            />
          </div>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="policy-urg-heavy-tokens">{t('admin:poolUrgencyHeavyTokens')}</Label>
            <Input
              id="policy-urg-heavy-tokens"
              type="number"
              min={0}
              placeholder="32000"
              value={policy.selection?.urgency?.heavy_prompt_tokens ?? ''}
              onChange={(e) =>
                onChange({
                  ...policy,
                  selection: {
                    ...policy.selection,
                    urgency: {
                      ...policy.selection?.urgency,
                      heavy_prompt_tokens: parseOptInt(e.target.value),
                    },
                  },
                })
              }
            />
          </div>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="policy-urg-heavy-min">{t('admin:poolUrgencyHeavyMinHeadroom')}</Label>
            <Input
              id="policy-urg-heavy-min"
              type="number"
              min={0}
              max={90}
              placeholder="20"
              value={policy.selection?.urgency?.heavy_min_headroom_pct ?? ''}
              onChange={(e) =>
                onChange({
                  ...policy,
                  selection: {
                    ...policy.selection,
                    urgency: {
                      ...policy.selection?.urgency,
                      heavy_min_headroom_pct: parseOptInt(e.target.value),
                    },
                  },
                })
              }
            />
          </div>
        </div>
      </FieldGroup>

      <FieldGroup title={t('admin:poolAffinityFailoverGroup')} hint={t('admin:poolAffinityFailoverHint')}>
        <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="policy-affinity-mode">{t('admin:poolAffinityMode')}</Label>
            <Select
              id="policy-affinity-mode"
              value={policy.affinity?.mode ?? 'prefer_warm'}
              onChange={(v) =>
                onChange({
                  ...policy,
                  affinity: { ...policy.affinity, mode: v as AffinityMode },
                })
              }
              options={AFFINITY_MODES.map((m) => ({ value: m, label: t(AFFINITY_LABEL[m]) }))}
            />
          </div>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="policy-affinity-ttl">{t('admin:poolAffinityTtl')}</Label>
            <Input
              id="policy-affinity-ttl"
              type="number"
              min={60}
              max={86400}
              placeholder={t('admin:poolInheritGlobal')}
              value={policy.affinity?.ttl_secs ?? ''}
              onChange={(e) =>
                onChange({
                  ...policy,
                  affinity: { ...policy.affinity, ttl_secs: parseOptInt(e.target.value) },
                })
              }
            />
          </div>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="policy-max-attempts">{t('admin:poolMaxAttempts')}</Label>
            <Input
              id="policy-max-attempts"
              type="number"
              min={1}
              max={16}
              placeholder={t('admin:poolInheritGlobal')}
              value={policy.failover?.max_attempts ?? ''}
              onChange={(e) =>
                onChange({
                  ...policy,
                  failover: { ...policy.failover, max_attempts: parseOptInt(e.target.value) },
                })
              }
            />
          </div>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="policy-same-retries">{t('admin:poolMaxSameRetries')}</Label>
            <Input
              id="policy-same-retries"
              type="number"
              min={0}
              max={5}
              placeholder={t('admin:poolInheritGlobal')}
              value={policy.failover?.max_same_channel_retries ?? ''}
              onChange={(e) =>
                onChange({
                  ...policy,
                  failover: {
                    ...policy.failover,
                    max_same_channel_retries: parseOptInt(e.target.value),
                  },
                })
              }
            />
          </div>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="policy-first-timeout">{t('admin:poolFirstOutputTimeout')}</Label>
            <Input
              id="policy-first-timeout"
              type="number"
              min={1}
              max={600}
              placeholder={t('admin:poolInheritGlobal')}
              value={policy.failover?.first_output_timeout_secs ?? ''}
              onChange={(e) =>
                onChange({
                  ...policy,
                  failover: {
                    ...policy.failover,
                    first_output_timeout_secs: parseOptInt(e.target.value),
                  },
                })
              }
            />
          </div>
        </div>
      </FieldGroup>

      <FieldGroup title={t('admin:poolGuardrailsGroup')} hint={t('admin:poolGuardrailsHint')}>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="policy-max-cost">{t('admin:poolMaxCostMilli')}</Label>
          <Input
            id="policy-max-cost"
            type="number"
            min={0}
            max={100000}
            className="w-40 font-mono text-sm"
            placeholder={t('admin:groupRateUnlimited')}
            value={policy.guardrails?.max_cost_milli ?? ''}
            onChange={(e) =>
              onChange({
                ...policy,
                guardrails: {
                  ...policy.guardrails,
                  max_cost_milli: parseOptInt(e.target.value),
                },
              })
            }
          />
          <p className="text-xs text-muted-foreground">{t('admin:poolMaxCostMilliHint')}</p>
        </div>

        <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="policy-allowed-prov">{t('admin:poolAllowedProviders')}</Label>
            <TagInput
              id="policy-allowed-prov"
              value={policy.guardrails?.allowed_providers ?? []}
              onChange={(v) =>
                onChange({ ...policy, guardrails: { ...policy.guardrails, allowed_providers: v } })
              }
              placeholder={t('admin:poolProvidersPlaceholder')}
            />
          </div>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="policy-denied-prov">{t('admin:poolDeniedProviders')}</Label>
            <TagInput
              id="policy-denied-prov"
              value={policy.guardrails?.denied_providers ?? []}
              onChange={(v) =>
                onChange({ ...policy, guardrails: { ...policy.guardrails, denied_providers: v } })
              }
              placeholder={t('admin:poolProvidersPlaceholder')}
            />
          </div>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="policy-req-tags">{t('admin:poolRequiredTags')}</Label>
            <TagInput
              id="policy-req-tags"
              value={policy.guardrails?.required_tags ?? []}
              onChange={(v) =>
                onChange({ ...policy, guardrails: { ...policy.guardrails, required_tags: v } })
              }
              placeholder={t('admin:poolTagsPlaceholder')}
            />
          </div>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="policy-ex-tags">{t('admin:poolExcludedTags')}</Label>
            <TagInput
              id="policy-ex-tags"
              value={policy.guardrails?.excluded_tags ?? []}
              onChange={(v) =>
                onChange({ ...policy, guardrails: { ...policy.guardrails, excluded_tags: v } })
              }
              placeholder={t('admin:poolTagsPlaceholder')}
            />
          </div>
        </div>
      </FieldGroup>

      <FieldGroup title={t('admin:poolQualityGroup')} hint={t('admin:poolQualityHint')}>
        <Switch
          checked={policy.quality?.enabled === true}
          onChange={(enabled) =>
            onChange({ ...policy, quality: { ...policy.quality, enabled } })
          }
          label={t('admin:poolQualityEnabled')}
          description={t('admin:poolQualityEnabledDesc')}
        />
        <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="policy-qual-min">{t('admin:poolQualityMinSamples')}</Label>
            <Input
              id="policy-qual-min"
              type="number"
              min={1}
              max={1000}
              placeholder="10"
              value={policy.quality?.min_samples ?? ''}
              onChange={(e) =>
                onChange({
                  ...policy,
                  quality: { ...policy.quality, min_samples: parseOptInt(e.target.value) },
                })
              }
            />
          </div>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="policy-qual-demote">{t('admin:poolQualityDemoteErr')}</Label>
            <Input
              id="policy-qual-demote"
              type="number"
              min={1}
              max={1000}
              placeholder="150"
              value={policy.quality?.demote_error_rate_milli ?? ''}
              onChange={(e) =>
                onChange({
                  ...policy,
                  quality: {
                    ...policy.quality,
                    demote_error_rate_milli: parseOptInt(e.target.value),
                  },
                })
              }
            />
          </div>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="policy-qual-eject">{t('admin:poolQualityEjectErr')}</Label>
            <Input
              id="policy-qual-eject"
              type="number"
              min={1}
              max={1000}
              placeholder={t('admin:groupRateUnlimited')}
              value={policy.quality?.eject_error_rate_milli ?? ''}
              onChange={(e) =>
                onChange({
                  ...policy,
                  quality: {
                    ...policy.quality,
                    eject_error_rate_milli: parseOptInt(e.target.value),
                  },
                })
              }
            />
          </div>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="policy-qual-lat">{t('admin:poolQualityDemoteLatency')}</Label>
            <Input
              id="policy-qual-lat"
              type="number"
              min={100}
              max={300000}
              placeholder={t('admin:groupRateUnlimited')}
              value={policy.quality?.demote_latency_ms ?? ''}
              onChange={(e) =>
                onChange({
                  ...policy,
                  quality: {
                    ...policy.quality,
                    demote_latency_ms: parseOptInt(e.target.value),
                  },
                })
              }
            />
          </div>
        </div>
      </FieldGroup>

      <FieldGroup title={t('admin:poolCompactionGroup')} hint={t('admin:poolCompactionHint')}>
        <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
          <div className="flex flex-col gap-1.5 sm:col-span-2">
            <Label htmlFor="policy-compact-mode">{t('admin:poolCompactionMode')}</Label>
            <Select
              id="policy-compact-mode"
              value={policy.compaction?.mode ?? 'off'}
              onChange={(v) =>
                onChange({
                  ...policy,
                  compaction: { ...policy.compaction, mode: v as CompactionMode },
                })
              }
              options={COMPACTION_MODES.map((m) => ({ value: m, label: t(COMPACTION_LABEL[m]) }))}
            />
          </div>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="policy-compact-min-tok">{t('admin:poolCompactionMinTokens')}</Label>
            <Input
              id="policy-compact-min-tok"
              type="number"
              min={1000}
              max={500000}
              placeholder="16000"
              value={policy.compaction?.min_prompt_tokens ?? ''}
              onChange={(e) =>
                onChange({
                  ...policy,
                  compaction: {
                    ...policy.compaction,
                    min_prompt_tokens: parseOptInt(e.target.value),
                  },
                })
              }
            />
          </div>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="policy-compact-keep">{t('admin:poolCompactionKeepRecent')}</Label>
            <Input
              id="policy-compact-keep"
              type="number"
              min={2}
              max={64}
              placeholder="6"
              value={policy.compaction?.keep_recent_messages ?? ''}
              onChange={(e) =>
                onChange({
                  ...policy,
                  compaction: {
                    ...policy.compaction,
                    keep_recent_messages: parseOptInt(e.target.value),
                  },
                })
              }
            />
          </div>
          <div className="flex flex-col gap-1.5 sm:col-span-2">
            <Label htmlFor="policy-compact-chars">{t('admin:poolCompactionMaxToolChars')}</Label>
            <Input
              id="policy-compact-chars"
              type="number"
              min={100}
              max={20000}
              className="w-40"
              placeholder="600"
              value={policy.compaction?.max_old_tool_chars ?? ''}
              onChange={(e) =>
                onChange({
                  ...policy,
                  compaction: {
                    ...policy.compaction,
                    max_old_tool_chars: parseOptInt(e.target.value),
                  },
                })
              }
            />
          </div>
        </div>
        <Switch
          checked={policy.compaction?.strip_stale_cache_breakpoints !== false}
          onChange={(strip_stale_cache_breakpoints) =>
            onChange({
              ...policy,
              compaction: { ...policy.compaction, strip_stale_cache_breakpoints },
            })
          }
          label={t('admin:poolCompactionStripBp')}
          description={t('admin:poolCompactionStripBpDesc')}
        />
      </FieldGroup>

      <FieldGroup title={t('admin:poolRulesGroup')} hint={t('admin:poolRulesHint')}>
        {rules.length === 0 ? (
          <p className="text-xs text-muted-foreground">{t('admin:poolRuleEmpty')}</p>
        ) : (
          <div className="flex flex-col gap-3">
            {rules.map((rule, idx) => (
              <div
                key={idx}
                className="flex flex-col gap-3 rounded-lg border border-border/85 bg-muted/20 p-3"
              >
                <div className="flex items-center justify-between gap-2">
                  <Input
                    className="h-8 max-w-56 font-mono text-xs"
                    aria-label={t('admin:poolRuleName')}
                    placeholder={t('admin:poolRuleName')}
                    value={rule.name}
                    onChange={(e) => updateRule(idx, { ...rule, name: e.target.value })}
                  />
                  <IconButton
                    icon={Trash2}
                    variant="destructive"
                    label={t('common:delete')}
                    onClick={() => removeRule(idx)}
                  />
                </div>

                <div className="grid grid-cols-1 gap-2.5 sm:grid-cols-2">
                  <div className="flex flex-col gap-1 sm:col-span-2">
                    <Label className="text-xs">{t('admin:poolRuleModels')}</Label>
                    <TagInput
                      value={rule.when.models ?? []}
                      onChange={(models) => updateRule(idx, { ...rule, when: { ...rule.when, models } })}
                      placeholder={t('admin:poolRuleModelsPlaceholder')}
                    />
                  </div>
                  <div className="flex flex-col gap-1">
                    <Label className="text-xs">{t('admin:poolRuleMinTokens')}</Label>
                    <Input
                      type="number"
                      min={0}
                      className="h-8 text-xs"
                      value={rule.when.min_prompt_tokens ?? ''}
                      onChange={(e) =>
                        updateRule(idx, {
                          ...rule,
                          when: { ...rule.when, min_prompt_tokens: parseOptInt(e.target.value) },
                        })
                      }
                    />
                  </div>
                  <div className="flex flex-col gap-1">
                    <Label className="text-xs">{t('admin:poolRuleMaxTokens')}</Label>
                    <Input
                      type="number"
                      min={0}
                      className="h-8 text-xs"
                      value={rule.when.max_prompt_tokens ?? ''}
                      onChange={(e) =>
                        updateRule(idx, {
                          ...rule,
                          when: { ...rule.when, max_prompt_tokens: parseOptInt(e.target.value) },
                        })
                      }
                    />
                  </div>
                  <div className="flex flex-col gap-1">
                    <Label className="text-xs">{t('admin:poolRuleHasTools')}</Label>
                    <Select
                      value={
                        rule.when.has_tools === true
                          ? 'yes'
                          : rule.when.has_tools === false
                            ? 'no'
                            : ''
                      }
                      onChange={(v) =>
                        updateRule(idx, {
                          ...rule,
                          when: { ...rule.when, has_tools: v === 'yes' ? true : v === 'no' ? false : null },
                        })
                      }
                      options={[
                        { value: '', label: t('admin:poolRuleToolsAny') },
                        { value: 'yes', label: t('admin:poolRuleToolsYes') },
                        { value: 'no', label: t('admin:poolRuleToolsNo') },
                      ]}
                    />
                  </div>
                  <div className="flex flex-col gap-1">
                    <Label className="text-xs">{t('admin:poolRuleOverrideStrategy')}</Label>
                    <Select
                      value={rule.apply.selection?.mode ?? ''}
                      onChange={(v) =>
                        updateRule(idx, {
                          ...rule,
                          apply: { ...rule.apply, selection: v === '' ? null : { mode: v } },
                        })
                      }
                      placeholder={t('admin:poolInheritGlobal')}
                      options={ROUTING_STRATEGIES.map((s) => ({ value: s, label: t(STRATEGY_LABEL[s]) }))}
                    />
                  </div>
                  <div className="flex flex-col gap-1">
                    <Label className="text-xs">{t('admin:poolRuleOverrideAffinity')}</Label>
                    <Select
                      value={rule.apply.affinity?.mode ?? ''}
                      onChange={(v) =>
                        updateRule(idx, {
                          ...rule,
                          apply: {
                            ...rule.apply,
                            affinity: v === '' ? null : { mode: v as AffinityMode },
                          },
                        })
                      }
                      placeholder={t('admin:poolInheritGlobal')}
                      options={AFFINITY_MODES.map((m) => ({ value: m, label: t(AFFINITY_LABEL[m]) }))}
                    />
                  </div>
                  <div className="flex flex-col gap-1">
                    <Label className="text-xs">{t('admin:poolRuleOverrideCanary')}</Label>
                    <Input
                      type="number"
                      min={0}
                      max={100}
                      className="h-8 text-xs"
                      placeholder={t('admin:poolInheritGlobal')}
                      value={rule.apply.canary_percent ?? ''}
                      onChange={(e) =>
                        updateRule(idx, {
                          ...rule,
                          apply: { ...rule.apply, canary_percent: parseOptInt(e.target.value) },
                        })
                      }
                    />
                  </div>
                  <div className="flex flex-col gap-1">
                    <Label className="text-xs">{t('admin:poolRuleOverrideMaxAttempts')}</Label>
                    <Input
                      type="number"
                      min={1}
                      max={16}
                      className="h-8 text-xs"
                      placeholder={t('admin:poolInheritGlobal')}
                      value={rule.apply.failover?.max_attempts ?? ''}
                      onChange={(e) => {
                        const v = parseOptInt(e.target.value)
                        updateRule(idx, {
                          ...rule,
                          apply: {
                            ...rule.apply,
                            failover: v === null ? null : { max_attempts: v },
                          },
                        })
                      }}
                    />
                  </div>
                  <div className="flex flex-col gap-1 sm:col-span-2">
                    <Label className="text-xs">{t('admin:poolRuleOverrideRequiredTags')}</Label>
                    <TagInput
                      value={rule.apply.guardrails?.required_tags ?? []}
                      onChange={(required_tags) =>
                        updateRule(idx, {
                          ...rule,
                          apply: {
                            ...rule.apply,
                            guardrails: required_tags.length > 0 ? { required_tags } : null,
                          },
                        })
                      }
                      placeholder={t('admin:poolTagsPlaceholder')}
                    />
                  </div>
                  <div className="flex flex-col gap-1 sm:col-span-2">
                    <Label className="text-xs">{t('admin:poolRuleOverridePools')}</Label>
                    <TagInput
                      value={rule.apply.override_pools ?? []}
                      onChange={(override_pools) =>
                        updateRule(idx, {
                          ...rule,
                          apply: {
                            ...rule.apply,
                            override_pools: override_pools.length > 0 ? override_pools : null,
                          },
                        })
                      }
                      suggestions={poolCodes}
                      placeholder={t('admin:poolRuleOverridePoolsPlaceholder')}
                    />
                  </div>
                </div>
              </div>
            ))}
          </div>
        )}
        <Button size="sm" variant="outline" className="self-start" onClick={addRule}>
          <Plus className="h-3.5 w-3.5" />
          {t('admin:poolRuleAdd')}
        </Button>
      </FieldGroup>
    </div>
  )
}
