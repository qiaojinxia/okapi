import { useMutation } from '@tanstack/react-query'
import { Play } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { FieldGroup } from '@/components/ui/drawer'
import { Input, Label } from '@/components/ui/input'
import { Switch } from '@/components/ui/switch'
import { toast } from '@/components/ui/toast'
import {
  AFFINITY_LABEL,
  ROLE_LABEL,
  STRATEGY_LABEL,
  normalizePolicyForSubmit,
  roleOf,
  strategyOf,
} from '@/features/pools/types'
import type {
  AffinityMode,
  DecisionTrace,
  PoolPolicy,
  SimulatePoolResponse,
} from '@/features/pools/types'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'

function TraceView({ title, trace }: { title: string; trace: DecisionTrace }) {
  const { t } = useTranslation()
  const eff = trace.effective_policy
  const affinityKey =
    eff.affinity_mode === 'strict' || eff.affinity_mode === 'off'
      ? AFFINITY_LABEL[eff.affinity_mode as AffinityMode]
      : AFFINITY_LABEL.prefer_warm

  const reasonLabel = (reason: string) => {
    const key = `admin:poolSimReason_${reason}`
    const translated = t(key)
    return translated === key ? reason : translated
  }

  return (
    <div className="flex flex-col gap-3 rounded-lg border border-border bg-muted/20 p-3.5">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <span className="text-xs font-semibold text-foreground">{title}</span>
        <div className="flex flex-wrap items-center gap-1.5">
          <Badge variant="muted" className="font-mono">
            {t('admin:poolPolicyVersion', { v: trace.policy_version })}
          </Badge>
          <Badge variant={trace.canary_selected ? 'default' : 'muted'}>
            {trace.canary_selected ? t('admin:poolSimCanaryHit') : t('admin:poolSimCanaryMiss')}
          </Badge>
        </div>
      </div>

      <div className="grid grid-cols-1 gap-2 rounded-md border border-border/70 bg-card/70 p-2.5 text-xs sm:grid-cols-2">
        <div>
          <span className="text-muted-foreground">{t('admin:poolSimPoolChain')}: </span>
          <span className="font-mono">{eff.pool_chain.join(' → ')}</span>
        </div>
        <div>
          <span className="text-muted-foreground">{t('admin:poolStrategy')}: </span>
          <span>{t(STRATEGY_LABEL[strategyOf(eff.strategy)])}</span>
        </div>
        <div>
          <span className="text-muted-foreground">{t('admin:poolAffinityMode')}: </span>
          <span>{t(affinityKey)}</span>
        </div>
        <div>
          <span className="text-muted-foreground">{t('admin:poolSimMatchedRules')}: </span>
          {eff.matched_rules.length === 0 ? (
            <span className="text-muted-foreground">{t('admin:poolSimNoRules')}</span>
          ) : (
            <span className="font-mono">{eff.matched_rules.join(', ')}</span>
          )}
        </div>
      </div>

      <div className="flex flex-col gap-1.5">
        <div className="text-xs font-medium text-foreground">
          {t('admin:poolSimOrdered', { n: trace.ordered_candidates.length })}
        </div>
        {trace.ordered_candidates.length === 0 ? (
          <p className="text-xs text-destructive">{t('admin:poolSimNoOrdered')}</p>
        ) : (
          <div className="flex flex-col gap-1">
            {trace.ordered_candidates.map((c, idx) => (
              <div
                key={`${c.channel_key_id}-${idx}`}
                className="flex flex-wrap items-center gap-2 rounded border border-border/70 bg-card px-2.5 py-1.5 text-xs"
              >
                <span className="w-5 font-mono text-muted-foreground">#{idx + 1}</span>
                <span className="font-medium text-foreground">{c.channel_name}</span>
                <Badge variant="muted" className="font-mono text-[10px]">
                  {c.provider}
                </Badge>
                <Badge variant={c.pool_role === 'canary' ? 'default' : 'muted'} className="text-[10px]">
                  {t(ROLE_LABEL[roleOf(c.pool_role)])}
                </Badge>
                {c.pool_rank > 0 && (
                  <Badge variant="muted" className="font-mono text-[10px]">
                    rank {c.pool_rank}
                  </Badge>
                )}
                {c.urgency_score !== undefined && (
                  <Badge variant="default" className="font-mono text-[10px]">
                    {t('admin:poolSimUrgencyBadge', {
                      score: c.urgency_score,
                      window: c.dominant_window ?? '—',
                    })}
                  </Badge>
                )}
                <span className="ml-auto font-mono text-[11px] text-muted-foreground">
                  P{c.priority} · W{c.weight} · {c.cost_milli}‰
                </span>
              </div>
            ))}
          </div>
        )}
      </div>

      {trace.rejected_candidates.length > 0 && (
        <div className="flex flex-col gap-1.5">
          <div className="text-xs font-medium text-muted-foreground">
            {t('admin:poolSimRejected', { n: trace.rejected_candidates.length })}
          </div>
          <div className="flex flex-col gap-1">
            {trace.rejected_candidates.map((r, idx) => (
              <div
                key={`${r.channel_key_id}-${idx}`}
                className="flex flex-wrap items-center justify-between gap-2 rounded border border-border/60 bg-card/50 px-2.5 py-1.5 text-xs"
              >
                <span className="text-muted-foreground">
                  {r.channel_name} ({r.provider})
                </span>
                <Badge variant="destructive" className="text-[10px]">
                  {reasonLabel(r.reason)}
                </Badge>
              </div>
            ))}
          </div>
        </div>
      )}
    </div>
  )
}

/// 策略模拟（Dry-Run）面板：调用 `POST /admin/pools/{code}/simulate`，支持对比已保存策略与当前草稿。
export function PoolSimulatePanel({
  poolCode,
  draftPolicy,
  suggestedModels,
}: {
  poolCode: string
  draftPolicy: PoolPolicy
  suggestedModels: string[]
}) {
  const { t } = useTranslation()
  const [model, setModel] = useState(suggestedModels[0] ?? '')
  const [promptTokens, setPromptTokens] = useState('1000')
  const [hasTools, setHasTools] = useState(false)
  const [sessionKey, setSessionKey] = useState('')
  const [compareDraft, setCompareDraft] = useState(true)

  const run = useMutation({
    mutationFn: () =>
      apiFetch<SimulatePoolResponse>(`/admin/pools/${encodeURIComponent(poolCode)}/simulate`, {
        method: 'POST',
        body: {
          model: model.trim(),
          prompt_tokens: Math.max(0, Number.parseInt(promptTokens, 10) || 0),
          has_tools: hasTools,
          session_key: sessionKey.trim() === '' ? null : sessionKey.trim(),
          draft_policy: compareDraft ? normalizePolicyForSubmit(draftPolicy) : null,
        },
      }),
    onError: (err) => toast.error(describeError(err)),
  })

  return (
    <div className="flex flex-col gap-4">
      <FieldGroup title={t('admin:poolSimTitle')} hint={t('admin:poolSimHint')}>
        <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
          <div className="flex flex-col gap-1.5 sm:col-span-2">
            <Label htmlFor="sim-model">{t('admin:poolSimModel')}</Label>
            <Input
              id="sim-model"
              className="font-mono text-sm"
              list={`sim-models-${poolCode}`}
              placeholder={t('admin:poolSimModelPlaceholder')}
              value={model}
              onChange={(e) => setModel(e.target.value)}
            />
            {suggestedModels.length > 0 && (
              <datalist id={`sim-models-${poolCode}`}>
                {suggestedModels.map((m) => (
                  <option key={m} value={m} />
                ))}
              </datalist>
            )}
          </div>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="sim-tokens">{t('admin:poolSimPromptTokens')}</Label>
            <Input
              id="sim-tokens"
              type="number"
              min={0}
              value={promptTokens}
              onChange={(e) => setPromptTokens(e.target.value)}
            />
          </div>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="sim-session">{t('admin:poolSimSessionKey')}</Label>
            <Input
              id="sim-session"
              className="font-mono text-sm"
              placeholder={t('admin:poolSimSessionPlaceholder')}
              value={sessionKey}
              onChange={(e) => setSessionKey(e.target.value)}
            />
          </div>
        </div>

        <Switch
          checked={hasTools}
          onChange={setHasTools}
          label={t('admin:poolSimHasTools')}
        />
        <Switch
          checked={compareDraft}
          onChange={setCompareDraft}
          label={t('admin:poolSimCompareDraft')}
        />

        <Button
          size="sm"
          className="self-start"
          disabled={model.trim() === '' || run.isPending}
          onClick={() => run.mutate()}
        >
          <Play className="h-3.5 w-3.5" />
          {t('admin:poolSimRun')}
        </Button>
      </FieldGroup>

      {run.data && (
        <div className="flex flex-col gap-3">
          <TraceView title={t('admin:poolSimSaved')} trace={run.data.saved} />
          {run.data.draft && (
            <TraceView title={t('admin:poolSimDraft')} trace={run.data.draft} />
          )}
        </div>
      )}
    </div>
  )
}
