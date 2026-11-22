import { useMutation, useQuery } from '@tanstack/react-query'
import { Plus } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import { Input } from '@/components/ui/input'
import { OptionalSection } from '@/components/ui/optional-section'
import { Select } from '@/components/ui/select'
import { ErrorState } from '@/components/ui/state'
import { Switch } from '@/components/ui/switch'
import { TagInput } from '@/components/ui/tag-input'
import { toast } from '@/components/ui/toast'
import { PoolCreateInline } from '@/features/pools/PoolCreateInline'
import { poolOptions } from '@/features/pools/pool-options'
import { DEFAULT_POOL, MEMBER_ROLES, ROLE_LABEL, roleOf } from '@/features/pools/types'
import type { MemberRole, PoolMember } from '@/features/pools/types'
import { usePermission } from '@/hooks/use-auth'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { parseInt32 } from '@/lib/int32'

/// 缺省成员关系：只进 default 池、不覆盖（role/enabled/tags/model_filter 缺省由后端默认补齐）。
export function defaultMembership(): PoolMember[] {
  return [{ pool_code: DEFAULT_POOL, priority_override: null, weight_override: null }]
}

/// 覆盖值输入：保留原始文本，输入中途的 "-"、"1a" 不会把整格清空；只有留空（继承）或合法的
/// 32 位整数才写回，否则标红，失焦后恢复成最后一次合法的值——保存的永远是看得见的那个数。
function OverrideInput({
  value,
  placeholder,
  onChange,
}: {
  value: number | null
  placeholder: string
  onChange: (next: number | null) => void
}) {
  const [draft, setDraft] = useState<string | null>(null)
  const text = draft ?? (value === null ? '' : String(value))
  return (
    <Input
      className="h-7 w-16 px-2 text-xs"
      inputMode="numeric"
      placeholder={placeholder}
      value={text}
      aria-invalid={parseInt32(text) === null}
      onChange={(e) => {
        setDraft(e.target.value)
        const parsed = parseInt32(e.target.value)
        if (parsed !== null) onChange(parsed ?? null)
      }}
      onBlur={() => setDraft(null)}
    />
  )
}

/// 调度池成员关系编辑器（受控）：每个池一行——勾选即加入；勾上后可配置
/// 角色（主力/备选/灰度）、单池启用开关、优先级/权重覆盖、成员标签与模型通配过滤。
export function PoolMembershipEditor({
  value,
  onChange,
}: {
  value: PoolMember[]
  onChange: (next: PoolMember[]) => void
}) {
  const { t } = useTranslation()
  const can = usePermission()
  const [creating, setCreating] = useState(false)
  const pools = useQuery(poolOptions())
  const list = pools.data?.data ?? []
  const find = (code: string) => value.find((m) => m.pool_code === code)

  const toggle = (code: string) => {
    if (find(code)) onChange(value.filter((m) => m.pool_code !== code))
    else onChange([...value, { pool_code: code, priority_override: null, weight_override: null }])
  }
  const update = (code: string, patch: Partial<PoolMember>) => {
    onChange(value.map((m) => (m.pool_code === code ? { ...m, ...patch } : m)))
  }

  if (pools.isError && !pools.data) {
    return <ErrorState message={describeError(pools.error)} onRetry={() => void pools.refetch()} />
  }
  if (pools.isPending) {
    return <p className="text-xs text-muted-foreground">{t('common:loading')}</p>
  }
  return (
    <div className="flex flex-col gap-2">
      {pools.isError && !creating && (
        <ErrorState message={describeError(pools.error)} onRetry={() => void pools.refetch()} />
      )}
      {list.length === 0 && <p className="text-xs text-muted-foreground">{t('common:empty')}</p>}
      {list.length > 0 && list.every((p) => p.builtin) && (
        <p className="text-xs leading-5 text-muted-foreground">{t('admin:poolOnlyBuiltin')}</p>
      )}
      {list.map((p) => {
        const m = find(p.pool_code)
        return (
          <div
            key={p.pool_code}
            className={
              m !== undefined
                ? 'flex flex-col gap-2.5 rounded-md border border-primary/35 bg-primary/[0.035] px-3 py-2.5 shadow-xs transition-colors'
                : 'flex flex-wrap items-center gap-3 rounded-md border border-border/85 bg-card px-3 py-2.5 transition-colors hover:border-muted-foreground/35'
            }
          >
            <div className="flex flex-wrap items-center gap-3">
              <Checkbox
                label={p.pool_code === DEFAULT_POOL ? t('admin:poolDefaultOption') : p.pool_code}
                checked={m !== undefined}
                className="min-w-40 font-mono text-xs"
                onChange={() => toggle(p.pool_code)}
              />
              {m !== undefined && (
                <div className="flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
                  <label className="flex items-center gap-1">
                    {t('admin:poolMemberRole')}
                    <Select
                      className="h-7 w-24 text-xs"
                      value={roleOf(m.role)}
                      onChange={(next) => update(p.pool_code, { role: next as MemberRole })}
                      options={MEMBER_ROLES.map((r) => ({ value: r, label: t(ROLE_LABEL[r]) }))}
                    />
                  </label>
                  <label className="flex items-center gap-1">
                    {t('admin:poolOverridePriority')}
                    <OverrideInput
                      value={m.priority_override}
                      placeholder={t('admin:poolOverrideInherit')}
                      onChange={(next) => update(p.pool_code, { priority_override: next })}
                    />
                  </label>
                  <label className="flex items-center gap-1">
                    {t('admin:poolOverrideWeight')}
                    <OverrideInput
                      value={m.weight_override}
                      placeholder={t('admin:poolOverrideInherit')}
                      onChange={(next) => update(p.pool_code, { weight_override: next })}
                    />
                  </label>
                  <Switch
                    checked={m.enabled !== false}
                    onChange={(enabled) => update(p.pool_code, { enabled })}
                    label={t('admin:poolMemberEnabled')}
                    className="gap-2 text-xs"
                  />
                </div>
              )}
              {p.fallback_pool_code && (
                <Badge variant="muted" className="ml-auto">
                  {t('admin:poolReachFallback', { pool: p.fallback_pool_code })}
                </Badge>
              )}
            </div>

            {m !== undefined && (
              <div className="grid grid-cols-1 gap-2 border-t border-border/50 pt-2 sm:grid-cols-2">
                <div className="flex flex-col gap-1">
                  <span className="text-[11px] text-muted-foreground">{t('admin:poolMemberTags')}</span>
                  <TagInput
                    value={m.tags ?? []}
                    onChange={(tags) => update(p.pool_code, { tags })}
                    placeholder={t('admin:poolTagsPlaceholder')}
                  />
                </div>
                <div className="flex flex-col gap-1">
                  <span className="text-[11px] text-muted-foreground">
                    {t('admin:poolMemberModelFilter')}
                  </span>
                  <TagInput
                    value={m.model_filter ?? []}
                    onChange={(listNext) =>
                      update(p.pool_code, { model_filter: listNext.length > 0 ? listNext : null })
                    }
                    placeholder={t('admin:poolMemberModelFilterPlaceholder')}
                  />
                </div>
              </div>
            )}
          </div>
        )
      })}
      {can('channel.write') &&
        (creating ? (
          <PoolCreateInline
            pools={list}
            onCancel={() => setCreating(false)}
            onCreated={(code) => {
              if (!find(code))
                onChange([...value, { pool_code: code, priority_override: null, weight_override: null }])
              setCreating(false)
            }}
          />
        ) : (
          <Button size="sm" variant="outline" className="self-start" onClick={() => setCreating(true)}>
            <Plus className="h-3.5 w-3.5" />
            {t('admin:poolCreate')}
          </Button>
        ))}
    </div>
  )
}

/// 编辑态：编辑器 + 单独保存（成员关系是独立端点，审计为 channel.set_pools）。
export function PoolMembership({
  channelId,
  current,
  defaultOpen,
  onDone,
}: {
  channelId: number
  current: PoolMember[]
  defaultOpen?: boolean
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

  const memberSummary = (member: PoolMember) => {
    const customized =
      member.priority_override !== null ||
      member.weight_override !== null ||
      (member.role !== undefined && member.role !== 'primary') ||
      member.enabled === false ||
      (member.tags !== undefined && member.tags.length > 0) ||
      (member.model_filter !== null && member.model_filter !== undefined && member.model_filter.length > 0)
    return member.pool_code + (customized ? ` (${t('admin:channelOptionsCustomized')})` : '')
  }

  return (
    <OptionalSection
      id="channel-pools"
      title={t('admin:poolMembership')}
      hint={t('admin:poolMembershipHint')}
      defaultOpen={defaultOpen}
      summary={members.map(memberSummary).join(', ') || t('admin:channelPoolsEmpty')}
      error={members.length === 0 ? t('admin:poolOrphanWarning') : undefined}
    >
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
    </OptionalSection>
  )
}
