import { useMutation, useQuery } from '@tanstack/react-query'
import { Layers, Play, Settings2, SlidersHorizontal, X } from 'lucide-react'
import { useId, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Drawer, FieldGroup } from '@/components/ui/drawer'
import { Input, Label } from '@/components/ui/input'
import { Select } from '@/components/ui/select'
import { TabPanel, Tabs } from '@/components/ui/tabs'
import { toast } from '@/components/ui/toast'
import { PoolPolicyEditor } from '@/features/pools/PoolPolicyEditor'
import { PoolReach } from '@/features/pools/PoolReach'
import { PoolSimulatePanel } from '@/features/pools/PoolSimulatePanel'
import {
  ROLE_LABEL,
  ROUTING_STRATEGIES,
  STRATEGY_HINT,
  STRATEGY_LABEL,
  emptyPolicy,
  normalizePolicyForSubmit,
  roleOf,
  strategyOf,
} from '@/features/pools/types'
import type { PoolDetail, PoolPolicy, PoolRow, RoutingStrategy } from '@/features/pools/types'
import { apiFetch } from '@/lib/api'
import { isMachineCode } from '@/lib/codes'
import { describeError } from '@/lib/i18n'
import { qk } from '@/lib/query-keys'
import { poolOptions } from './pool-options'

export type PoolDrawerTab = 'basic' | 'policy' | 'members' | 'simulate'

function initialPolicyOf(pool: PoolRow | undefined): PoolPolicy {
  const base = emptyPolicy()
  const raw = pool?.policy
  const fallbackPools =
    raw?.fallback_pools && raw.fallback_pools.length > 0
      ? [...raw.fallback_pools]
      : pool?.fallback_pool_code
        ? [pool.fallback_pool_code]
        : []
  return {
    ...base,
    ...raw,
    fallback_pools: fallbackPools,
    selection: { ...base.selection, ...raw?.selection },
    affinity: { ...base.affinity, ...raw?.affinity },
    failover: { ...base.failover, ...raw?.failover },
    guardrails: { ...base.guardrails, ...raw?.guardrails },
    quality: { ...base.quality, ...raw?.quality },
    compaction: { ...base.compaction, ...raw?.compaction },
    canary_percent: raw?.canary_percent ?? 0,
    rules: raw?.rules ? raw.rules.map((r) => ({ ...r, when: { ...r.when }, apply: { ...r.apply } })) : [],
  }
}

export function PoolDrawer({
  pool,
  initialTab = 'basic',
  onClose,
  onDone,
}: {
  pool: PoolRow | undefined
  initialTab?: PoolDrawerTab
  onClose: () => void
  onDone: () => void
}) {
  const { t } = useTranslation()
  const tabsId = useId()
  const isEdit = pool !== undefined
  const [tab, setTab] = useState<PoolDrawerTab>(isEdit ? initialTab : 'basic')
  const [code, setCode] = useState(pool?.pool_code ?? '')
  const [description, setDescription] = useState(pool?.description ?? '')
  const [strategy, setStrategy] = useState<RoutingStrategy>(strategyOf(pool?.routing_strategy))
  const [policy, setPolicy] = useState<PoolPolicy>(() => initialPolicyOf(pool))

  // 降级目标候选：其它调度池（不能选自己）
  const pools = useQuery(poolOptions())
  const detail = useQuery({
    queryKey: qk.poolDetail(pool?.pool_code ?? ''),
    queryFn: () => apiFetch<PoolDetail>(`/admin/pools/${encodeURIComponent(pool?.pool_code ?? '')}`),
    enabled: isEdit,
    staleTime: 15_000,
  })

  const allPoolCodes = (pools.data?.data ?? []).map((p) => p.pool_code)
  const fallbackChain = policy.fallback_pools ?? []
  const availableFallbacks = allPoolCodes
    .filter((c) => c !== code.trim() && !fallbackChain.includes(c))
    .map((c) => ({ value: c, label: c }))

  const codeExists = !isEdit && allPoolCodes.includes(code.trim())
  const codeInvalid = !isEdit && code.trim() !== '' && !isMachineCode(code)

  const save = useMutation({
    mutationFn: () => {
      const normalized = normalizePolicyForSubmit(policy)
      return apiFetch<{ ok: boolean; policy_version: number }>('/admin/pools', {
        method: 'POST',
        body: {
          pool_code: code.trim(),
          description,
          routing_strategy: strategy,
          fallback_pool_code: normalized.fallback_pools?.[0] ?? null,
          policy: normalized,
        },
      })
    },
    onSuccess: () => {
      onDone()
      onClose()
    },
    onError: (err) => toast.error(describeError(err)),
  })

  const tabItems = [
    { id: 'basic', label: t('admin:poolTabBasic'), icon: Settings2, panelId: `${tabsId}-panel-basic` },
    {
      id: 'policy',
      label: t('admin:poolTabPolicy'),
      icon: SlidersHorizontal,
      count: (policy.rules ?? []).length > 0 ? (policy.rules ?? []).length : undefined,
      panelId: `${tabsId}-panel-policy`,
    },
    ...(isEdit
      ? [
          {
            id: 'members',
            label: t('admin:poolTabMembers'),
            icon: Layers,
            count: detail.data?.members?.length ?? pool.channel_count,
            panelId: `${tabsId}-panel-members`,
          },
          {
            id: 'simulate',
            label: t('admin:poolTabSimulate'),
            icon: Play,
            panelId: `${tabsId}-panel-simulate`,
          },
        ]
      : []),
  ]

  return (
    <Drawer
      open
      size="lg"
      onClose={onClose}
      title={pool ? t('admin:poolEdit', { name: pool.pool_code }) : t('admin:poolCreate')}
      description={t('admin:poolDrawerDesc')}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>
            {t('common:cancel')}
          </Button>
          <Button
            disabled={code.trim() === '' || codeExists || codeInvalid || save.isPending}
            onClick={() => save.mutate()}
          >
            {t('common:save')}
          </Button>
        </>
      }
    >
      <Tabs
        id={tabsId}
        items={tabItems}
        active={tab}
        onChange={(id) => setTab(id as PoolDrawerTab)}
        variant="underline"
        className="mb-4"
      />

      <TabPanel id={`${tabsId}-panel-basic`} labelledBy={`${tabsId}-basic`} active={tab === 'basic'}>
        <div className="flex flex-col gap-5">
          <FieldGroup title={t('common:basicInfo')} hint={t('admin:poolCodeHint')}>
            <div className="flex flex-col gap-1.5">
              <Label htmlFor="pool-code">{t('admin:poolCode')}</Label>
              <Input
                id="pool-code"
                className="font-mono text-sm"
                value={code}
                readOnly={isEdit}
                placeholder="stable"
                maxLength={32}
                aria-invalid={codeExists || codeInvalid}
                onChange={(e) => setCode(e.target.value)}
              />
              {codeExists && (
                <p role="alert" className="text-xs text-destructive">
                  {t('admin:poolCodeExists')}
                </p>
              )}
              {codeInvalid && (
                <p role="alert" className="text-xs text-destructive">
                  {t('admin:codeFormatInvalid', { max: 32 })}
                </p>
              )}
            </div>
            <div className="flex flex-col gap-1.5">
              <Label htmlFor="pool-desc">{t('common:description')}</Label>
              <Input
                id="pool-desc"
                value={description}
                onChange={(e) => setDescription(e.target.value)}
              />
            </div>
          </FieldGroup>

          <FieldGroup title={t('admin:poolStrategy')} hint={t('admin:poolStrategyHint')}>
            <Select
              id="pool-strategy"
              className="w-60"
              value={strategy}
              onChange={(v) => setStrategy(strategyOf(v))}
              options={ROUTING_STRATEGIES.map((s) => ({ value: s, label: t(STRATEGY_LABEL[s]) }))}
            />
            <p className="text-xs text-muted-foreground">{t(STRATEGY_HINT[strategy])}</p>
          </FieldGroup>

          <FieldGroup title={t('admin:poolFallback')} hint={t('admin:poolFallbackHint')}>
            {fallbackChain.length > 0 && (
              <div className="flex flex-wrap items-center gap-1.5">
                {fallbackChain.map((fb, idx) => (
                  <span
                    key={fb}
                    className="inline-flex items-center gap-1 rounded-md border border-border bg-muted/60 px-2 py-1 font-mono text-xs"
                  >
                    <span className="text-[10px] text-muted-foreground">{idx + 1}.</span>
                    {fb}
                    <button
                      type="button"
                      aria-label={t('common:delete')}
                      className="ml-0.5 rounded p-0.5 text-muted-foreground hover:bg-muted hover:text-foreground"
                      onClick={() =>
                        setPolicy({
                          ...policy,
                          fallback_pools: fallbackChain.filter((x) => x !== fb),
                        })
                      }
                    >
                      <X className="h-3 w-3" />
                    </button>
                  </span>
                ))}
              </div>
            )}
            <Select
              id="pool-fallback-add"
              className="w-60"
              value=""
              onChange={(next) => {
                if (next && !fallbackChain.includes(next)) {
                  setPolicy({ ...policy, fallback_pools: [...fallbackChain, next] })
                }
              }}
              placeholder={
                fallbackChain.length === 0 ? t('admin:poolFallbackNone') : t('admin:poolFallbackAdd')
              }
              options={availableFallbacks}
            />
          </FieldGroup>

          {isEdit && <PoolReach poolCode={pool.pool_code} />}
        </div>
      </TabPanel>

      <TabPanel id={`${tabsId}-panel-policy`} labelledBy={`${tabsId}-policy`} active={tab === 'policy'}>
        <PoolPolicyEditor policy={policy} onChange={setPolicy} poolCodes={allPoolCodes} />
      </TabPanel>

      {isEdit && (
        <TabPanel
          id={`${tabsId}-panel-members`}
          labelledBy={`${tabsId}-members`}
          active={tab === 'members'}
        >
          <FieldGroup title={t('admin:poolTabMembers')} hint={t('admin:poolMembersHint')}>
            {detail.isPending ? (
              <p className="text-xs text-muted-foreground">{t('common:loading')}</p>
            ) : (detail.data?.members?.length ?? 0) === 0 ? (
              <p className="text-xs text-muted-foreground">{t('admin:poolMembersEmpty')}</p>
            ) : (
              <div className="flex flex-col gap-2">
                {(detail.data?.members ?? []).map((m) => {
                  const role = roleOf(m.role)
                  const enabledInPool = m.enabled !== false
                  return (
                    <div
                      key={m.channel_id}
                      className="flex flex-col gap-1.5 rounded-md border border-border/80 bg-card px-3 py-2.5 text-xs"
                    >
                      <div className="flex flex-wrap items-center gap-2">
                        <span className="font-medium text-foreground">{m.name}</span>
                        <Badge variant="muted" className="font-mono text-[10px]">
                          {m.provider}
                        </Badge>
                        <Badge variant={role === 'canary' ? 'default' : 'muted'} className="text-[10px]">
                          {t(ROLE_LABEL[role])}
                        </Badge>
                        {!enabledInPool && (
                          <Badge variant="destructive" className="text-[10px]">
                            {t('admin:poolMemberDrained')}
                          </Badge>
                        )}
                        <span className="ml-auto font-mono text-[11px] text-muted-foreground">
                          P{m.priority_override ?? m.priority}
                          {m.weight_override !== null ? ` · W${m.weight_override}` : ''} ·{' '}
                          {m.active_keys} keys
                        </span>
                      </div>
                      {((m.tags && m.tags.length > 0) ||
                        (m.model_filter && m.model_filter.length > 0)) && (
                        <div className="flex flex-wrap items-center gap-1.5 text-[11px] text-muted-foreground">
                          {(m.tags ?? []).map((tag) => (
                            <Badge key={tag} variant="muted" className="font-mono text-[10px]">
                              #{tag}
                            </Badge>
                          ))}
                          {m.model_filter && m.model_filter.length > 0 && (
                            <span className="font-mono">
                              filter: {m.model_filter.join(', ')}
                            </span>
                          )}
                        </div>
                      )}
                    </div>
                  )
                })}
              </div>
            )}
          </FieldGroup>
        </TabPanel>
      )}

      {isEdit && (
        <TabPanel
          id={`${tabsId}-panel-simulate`}
          labelledBy={`${tabsId}-simulate`}
          active={tab === 'simulate'}
        >
          <PoolSimulatePanel
            poolCode={pool.pool_code}
            draftPolicy={policy}
            suggestedModels={detail.data?.models ?? []}
          />
        </TabPanel>
      )}
    </Drawer>
  )
}
