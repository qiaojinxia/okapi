import { useMutation, useQuery } from '@tanstack/react-query'
import { AlertTriangle } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import { FieldGroup } from '@/components/ui/drawer'
import { Input } from '@/components/ui/input'
import type { PoolMember, PoolRow } from '@/features/pools/types'
import { DEFAULT_POOL } from '@/features/pools/types'
import { toast } from '@/components/ui/toast'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { qk } from '@/lib/query-keys'

/// 缺省成员关系：只进 default 池、不覆盖。
export function defaultMembership(): PoolMember[] {
  return [{ pool_code: DEFAULT_POOL, priority_override: null, weight_override: null }]
}

function parseOverride(raw: string): number | null {
  const n = Number(raw.trim())
  return raw.trim() === '' || !Number.isInteger(n) ? null : n
}

/// 池成员关系编辑器（受控）：每个池一行——勾选即加入；勾上后可给本池单独的
/// 优先级 / 权重覆盖（留空 = 继承渠道 / key 自身）。
///
/// 渠道只服务它所在的池：一个都不勾 = 孤儿渠道，对谁都不可达，编辑器就地红字提醒
/// 而不是等用户去日志里找 no_available_channel。新建与编辑共用这一个组件，
/// 提交时机由父级决定（新建随建渠道一起提交；编辑单独保存，审计语义不同）。
export function PoolMembershipEditor({
  value,
  onChange,
}: {
  value: PoolMember[]
  onChange: (next: PoolMember[]) => void
}) {
  const { t } = useTranslation()
  const pools = useQuery({
    queryKey: qk.adminPools,
    queryFn: () => apiFetch<{ data: PoolRow[] }>('/admin/pools'),
  })
  const list = pools.data?.data ?? []
  const find = (code: string) => value.find((m) => m.pool_code === code)

  const toggle = (code: string) => {
    if (find(code)) onChange(value.filter((m) => m.pool_code !== code))
    else onChange([...value, { pool_code: code, priority_override: null, weight_override: null }])
  }
  const patch = (code: string, field: 'priority_override' | 'weight_override', raw: string) => {
    onChange(value.map((m) => (m.pool_code === code ? { ...m, [field]: parseOverride(raw) } : m)))
  }

  if (list.length === 0) {
    return <p className="text-xs text-muted-foreground">{t('common:loading')}</p>
  }
  return (
    <div className="flex flex-col gap-2">
      {list.map((p) => {
        const m = find(p.pool_code)
        return (
          <div key={p.pool_code} className="flex flex-wrap items-center gap-3 rounded-md border border-border px-3 py-2">
            <Checkbox
              label={p.pool_code === DEFAULT_POOL ? t('admin:poolDefaultOption') : p.pool_code}
              checked={m !== undefined}
              className="min-w-40 font-mono text-xs"
              onChange={() => toggle(p.pool_code)}
            />
            {m !== undefined && (
              <div className="flex items-center gap-2 text-xs text-muted-foreground">
                <label className="flex items-center gap-1">
                  {t('admin:poolOverridePriority')}
                  <Input
                    className="h-7 w-16 px-2 text-xs"
                    inputMode="numeric"
                    placeholder={t('admin:poolOverrideInherit')}
                    value={m.priority_override ?? ''}
                    onChange={(e) => patch(p.pool_code, 'priority_override', e.target.value)}
                  />
                </label>
                <label className="flex items-center gap-1">
                  {t('admin:poolOverrideWeight')}
                  <Input
                    className="h-7 w-16 px-2 text-xs"
                    inputMode="numeric"
                    placeholder={t('admin:poolOverrideInherit')}
                    value={m.weight_override ?? ''}
                    onChange={(e) => patch(p.pool_code, 'weight_override', e.target.value)}
                  />
                </label>
              </div>
            )}
            {p.fallback_pool_code && (
              <Badge variant="muted" className="ml-auto">
                {t('admin:poolReachFallback', { pool: p.fallback_pool_code })}
              </Badge>
            )}
          </div>
        )
      })}
      {value.length === 0 && (
        <p className="flex items-center gap-1.5 text-xs text-destructive">
          <AlertTriangle className="h-3.5 w-3.5" />
          {t('admin:poolOrphanWarning')}
        </p>
      )}
    </div>
  )
}

/// 编辑态：编辑器 + 单独保存（成员关系是独立端点，审计为 channel.set_pools）。
export function PoolMembership({
  channelId,
  current,
  onDone,
}: {
  channelId: number
  current: PoolMember[]
  onDone: () => void
}) {
  const { t } = useTranslation()
  const [members, setMembers] = useState<PoolMember[]>(current)

  const save = useMutation({
    mutationFn: () =>
      apiFetch<{ ok: boolean; orphan: boolean }>(`/admin/channels/${channelId}/pools`, {
        method: 'POST',
        body: { pools: members },
      }),
    onSuccess: (r) => {
      toast.success(r.orphan ? t('admin:poolsSavedOrphan') : t('admin:poolsSaved'))
      onDone()
    },
    onError: (err) => toast.error(describeError(err)),
  })

  return (
    <FieldGroup title={t('admin:poolMembership')} hint={t('admin:poolMembershipHint')}>
      <PoolMembershipEditor value={members} onChange={setMembers} />
      <Button
        size="sm"
        variant="outline"
        className="self-start"
        disabled={save.isPending}
        onClick={() => save.mutate()}
      >
        {t('admin:savePools')}
      </Button>
    </FieldGroup>
  )
}
