import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { ArrowDown, ArrowUp, X } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { AutocompleteInput } from '@/components/ui/autocomplete-input'
import { Badge } from '@/components/ui/badge'
import { FieldGroup } from '@/components/ui/drawer'
import { IconButton } from '@/components/ui/icon-button'
import { Label } from '@/components/ui/input'
import { ErrorState, LoadingState } from '@/components/ui/state'
import { toast } from '@/components/ui/toast'
import type { GroupListRow } from '@/features/groups/types'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { formatRatio } from '@/lib/money'
import { qk } from '@/lib/query-keys'

export function GroupsSection({
  userId,
  current,
  ready = true,
  onDone,
}: {
  userId: number
  current: string[]
  ready?: boolean
  onDone: () => void
}) {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  // Late overview responses may supply current groups; do not initialise an empty draft first.
  // Once editing starts, a background refetch must not replace the administrator's choices.
  const [draft, setDraft] = useState<string[] | null>(null)
  const [search, setSearch] = useState('')
  const codes = draft ?? current
  const catalog = useQuery({
    queryKey: [...qk.adminGroups, 'assignment-options'],
    // Configuration lists return all groups when limit is omitted, not just page one.
    queryFn: () => apiFetch<{ data: GroupListRow[] }>('/admin/groups'),
    retry: false,
  })
  const byCode = new Map((catalog.data?.data ?? []).map((group) => [group.group_code, group]))
  const missing = catalog.isSuccess ? codes.filter((code) => !byCode.has(code)) : []
  const changed = codes.length !== current.length || codes.some((code, index) => code !== current[index])

  // 覆盖式设置分组：后端按 (group_code, priority) 全量替换，故 UI 也是全量提交
  const setGroups = useMutation({
    mutationFn: () => {
      if (!ready || !catalog.isSuccess || codes.some((code) => !byCode.has(code))) {
        throw new Error(t('admin:userGroupsInvalid'))
      }
      return apiFetch(`/admin/users/${userId}/groups`, {
        method: 'POST',
        body: {
          groups: codes.map((group_code, idx) => ({
            group_code,
            priority: codes.length - idx,
          })),
        },
      })
    },
    onSuccess: () => {
      toast.success(t('common:success'))
      onDone()
    },
    onError: (err) => {
      toast.error(describeError(err))
      void queryClient.invalidateQueries({ queryKey: [...qk.adminGroups, 'assignment-options'] })
    },
  })

  const disabled = !ready || !catalog.isSuccess || setGroups.isPending
  const move = (index: number, direction: -1 | 1) => {
    const next = [...codes]
    const target = index + direction
    if (disabled || target < 0 || target >= next.length) return
    ;[next[index], next[target]] = [next[target], next[index]]
    setDraft(next)
  }

  return (
    <FieldGroup title={t('admin:userGroups')} hint={t('admin:userGroupsHint')}>
      {!ready || catalog.isPending ? <LoadingState /> : catalog.isError ? (
        <ErrorState message={describeError(catalog.error)} onRetry={() => void catalog.refetch()} />
      ) : (
        <>
          <div className="flex min-w-0 flex-col gap-1.5">
            <Label htmlFor={`user-${userId}-group-search`}>{t('admin:userGroupsAdd')}</Label>
            <AutocompleteInput
              id={`user-${userId}-group-search`}
              value={search}
              onChange={setSearch}
              onChoose={(code) => {
                if (disabled || !byCode.has(code) || codes.includes(code)) return
                setDraft([...codes, code])
                setSearch('')
              }}
              options={(catalog.data?.data ?? []).filter((group) => !codes.includes(group.group_code)).map((group) => ({
                value: group.group_code,
                label: group.description ?? undefined,
                description: `${t('admin:userGroupsRatio', { ratio: formatRatio(group.group_ratio ?? '1') })} · ${t(group.self_select ? 'admin:userGroupsPublic' : 'admin:userGroupsAdminOnly')}`,
              }))}
              placeholder={t('admin:userGroupsSearch')}
              emptyHint={catalog.data?.data.length ? t('admin:userGroupsNoMatch') : t('admin:userGroupsNoCatalog')}
              disabled={setGroups.isPending}
              search
            />
          </div>
          <ol aria-label={t('admin:userGroupsSelected')} className="flex max-h-80 min-w-0 flex-col gap-2 overflow-y-auto overscroll-contain">
            {codes.map((code, index) => {
              const group = byCode.get(code)
              return <li key={code} className="flex min-w-0 items-center gap-2 rounded-lg border border-border bg-card p-2">
                <span className="flex h-6 w-6 shrink-0 items-center justify-center rounded bg-muted text-xs font-medium tabular-nums text-muted-foreground">{index + 1}</span>
                <div className="min-w-0 flex-1">
                  <span className="block break-all font-mono text-xs font-medium">{code}</span>
                  {group && <span className="mt-1 flex flex-wrap items-center gap-1 text-xs text-muted-foreground">
                    <span>{t('admin:userGroupsRatio', { ratio: formatRatio(group.group_ratio ?? '1') })}</span>
                    <Badge variant={group.self_select ? 'success' : 'muted'}>{t(group.self_select ? 'admin:userGroupsPublic' : 'admin:userGroupsAdminOnly')}</Badge>
                  </span>}
                </div>
                <div className="flex shrink-0 items-center gap-0.5">
                  <IconButton icon={ArrowUp} label={t('admin:userGroupsMoveUp', { code })} disabled={disabled || index === 0} onClick={() => move(index, -1)} />
                  <IconButton icon={ArrowDown} label={t('admin:userGroupsMoveDown', { code })} disabled={disabled || index === codes.length - 1} onClick={() => move(index, 1)} />
                  <IconButton icon={X} label={t('admin:userGroupsRemove', { code })} disabled={disabled} onClick={() => setDraft(codes.filter((item) => item !== code))} />
                </div>
              </li>
            })}
          </ol>
          {codes.length === 0 && <p className="rounded-lg border border-dashed border-border p-3 text-xs text-muted-foreground">{t('admin:userGroupsDefaultHint')}</p>}
          {missing.length > 0 && <p role="alert" className="text-xs text-destructive">{t('admin:userGroupsMissing', { codes: missing.join(', ') })}</p>}
          <p className="text-xs leading-5 text-muted-foreground">{t('admin:userGroupsPolicy')}</p>
        </>
      )}
      <Button
        size="sm"
        variant="outline"
        className="self-start"
        disabled={disabled || missing.length > 0 || !changed}
        loading={setGroups.isPending}
        onClick={() => setGroups.mutate()}
      >
        {t('common:save')}
      </Button>
    </FieldGroup>
  )
}
