import { useMutation, useQuery } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { Drawer, FieldGroup } from '@/components/ui/drawer'
import { Input, Label } from '@/components/ui/input'
import { Select } from '@/components/ui/select'
import { ROUTING_STRATEGIES, STRATEGY_HINT, STRATEGY_LABEL } from '@/features/pools/types'
import type { PoolRow } from '@/features/pools/types'
import { toast } from '@/components/ui/toast'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { qk } from '@/lib/query-keys'

type Strategy = (typeof ROUTING_STRATEGIES)[number]

export function PoolDrawer({
  pool,
  onClose,
  onDone,
}: {
  pool: PoolRow | undefined
  onClose: () => void
  onDone: () => void
}) {
  const { t } = useTranslation()
  const [code, setCode] = useState(pool?.pool_code ?? '')
  const [description, setDescription] = useState(pool?.description ?? '')
  const [strategy, setStrategy] = useState<Strategy>(
    (pool?.routing_strategy as Strategy | undefined) ?? 'priority_weighted',
  )
  const [fallback, setFallback] = useState(pool?.fallback_pool_code ?? '')

  // 降级目标候选：其它池（不能选自己）
  const pools = useQuery({
    queryKey: qk.adminPools,
    queryFn: () => apiFetch<{ data: PoolRow[] }>('/admin/pools'),
  })
  const fallbackOptions = (pools.data?.data ?? [])
    .filter((p) => p.pool_code !== code.trim())
    .map((p) => ({ value: p.pool_code, label: p.pool_code }))

  const save = useMutation({
    mutationFn: () =>
      apiFetch('/admin/pools', {
        method: 'POST',
        body: {
          pool_code: code.trim(),
          description,
          routing_strategy: strategy,
          fallback_pool_code: fallback === '' ? null : fallback,
        },
      }),
    onSuccess: () => {
      onDone()
      onClose()
    },
    onError: (err) => toast.error(describeError(err)),
  })

  return (
    <Drawer
      open
      onClose={onClose}
      title={pool ? t('admin:poolEdit', { name: pool.pool_code }) : t('admin:poolCreate')}
      description={t('admin:poolDrawerDesc')}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>
            {t('common:cancel')}
          </Button>
          <Button disabled={code.trim() === '' || save.isPending} onClick={() => save.mutate()}>
            {t('common:save')}
          </Button>
        </>
      }
    >
      <FieldGroup title={t('common:basicInfo')} hint={t('admin:poolCodeHint')}>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="pool-code">{t('admin:poolCode')}</Label>
          <Input
            id="pool-code"
            className="font-mono text-sm"
            value={code}
            readOnly={pool !== undefined}
            placeholder="stable"
            onChange={(e) => setCode(e.target.value)}
          />
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
          className="w-56"
          value={strategy}
          onChange={(v) => setStrategy(v as Strategy)}
          options={ROUTING_STRATEGIES.map((s) => ({ value: s, label: t(STRATEGY_LABEL[s]) }))}
        />
        <p className="text-xs text-muted-foreground">{t(STRATEGY_HINT[strategy])}</p>
      </FieldGroup>

      <FieldGroup title={t('admin:poolFallback')} hint={t('admin:poolFallbackHint')}>
        <Select
          id="pool-fallback"
          className="w-56"
          value={fallback}
          onChange={setFallback}
          placeholder={t('admin:poolFallbackNone')}
          options={fallbackOptions}
        />
      </FieldGroup>
    </Drawer>
  )
}
