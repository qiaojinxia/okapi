import { useQuery } from '@tanstack/react-query'
import { useEffect, useId, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { AutocompleteInput } from '@/components/ui/autocomplete-input'
import type { AutocompleteInputProps } from '@/components/ui/autocomplete-input'
import { usePermission } from '@/hooks/use-auth'
import { apiFetch } from '@/lib/api'
import { qk } from '@/lib/query-keys'
import { cn } from '@/lib/utils'

export type EntityKind = 'user' | 'api_key' | 'channel'
export interface EntityOption { id: number; name: string; description?: string }
export type EntitySearchInputProps = Omit<AutocompleteInputProps, 'options' | 'displayValue' | 'loading' | 'error' | 'optionLabelFirst' | 'emptyHint'> & {
  kind: EntityKind
  knownOptions?: EntityOption[]
}

// 缓存保留接口的原始响应，与管理列表共用首个分页。
interface DirectoryRow {
  id: number
  username?: string | null
  email?: string | null
  name?: string | null
  key_prefix?: string | null
  user_id?: number | null
  provider?: string | null
  api_base?: string | null
}

export function validEntityId(value: string): boolean {
  const text = value.trim()
  return text === '' || (/^\d+$/.test(text) && Number.isSafeInteger(Number(text)) && Number(text) > 0)
}

// 数字格式错误即时提示；尚未选中的检索名称，不把正常打字涂成错误。
export function malformedEntityId(value: string): boolean {
  return !validEntityId(value) && /\d/.test(value) && /^[\d.\-+eE\s]+$/.test(value)
}

export function EntitySearchInput(props: EntitySearchInputProps) {
  // 同一个 ID 不得把用户名称带到渠道输入框。
  return <EntitySearchField key={props.kind} {...props} />
}

function EntitySearchField({ kind, value, onChange, onChoose, knownOptions = [], className, onFocus, onBlur, ...props }: EntitySearchInputProps) {
  const { t } = useTranslation()
  const can = usePermission()
  const hintId = useId()
  const [focused, setFocused] = useState(false)
  const [composing, setComposing] = useState(false)
  const [editedValue, setEditedValue] = useState<string | null>(null)
  const [chosen, setChosen] = useState<EntityOption | null>(null)
  const [debounced, setDebounced] = useState<string | null>(null)
  const term = value.trim()
  const numeric = /^\d+$/.test(term)
  const allowed = kind === 'channel' ? can('channel.read') : can('user.read')
  const lookup = focused && !composing && !numeric && !malformedEntityId(term) && !props.disabled && !props.readOnly && allowed
  useEffect(() => {
    if (!lookup) return
    const timer = window.setTimeout(() => setDebounced(term), 250)
    return () => window.clearTimeout(timer)
  }, [lookup, term])
  const ready = lookup && term === debounced
  const query = debounced ?? ''
  const directory = useQuery({
    queryKey: kind === 'user' ? [...qk.adminUsers(query), 0, 20]
      : kind === 'api_key' ? [...qk.adminKeys(null, query), 0, 20]
        : [...qk.adminChannels, query, '', 0, 20],
    queryFn: () => {
      const params = new URLSearchParams({ limit: '20', offset: '0' })
      if (query) params.set('q', query)
      const endpoint = kind === 'user' ? 'users' : kind === 'api_key' ? 'keys' : 'channels'
      return apiFetch<{ data: DirectoryRow[]; total: number }>(`/admin/${endpoint}?${params}`)
    },
    enabled: ready,
    staleTime: 60_000,
    retry: false,
  })
  const remote = ready && !directory.isError ? directory.data?.data ?? [] : []
  const options = [...new Map([
    ...knownOptions,
    ...(chosen ? [chosen] : []),
    ...remote.map((row): EntityOption => ({
      id: row.id,
      name: (kind === 'user' ? row.username : row.name) || t(`flow:unnamed_${kind}`),
      description: kind === 'user' ? row.email ?? undefined
        : kind === 'channel' ? [row.provider, row.api_base].filter(Boolean).join(' · ')
          : [row.username || (row.user_id ? t('analytics:entityOwnerId', { id: row.user_id }) : undefined), row.key_prefix ? `${row.key_prefix}…` : undefined].filter(Boolean).join(' · '),
    })),
  ].filter((option) => Number.isSafeInteger(option.id) && option.id > 0).map((option) => [String(option.id), option])).values()]
  const selected = options.find((option) => String(option.id) === term)
  const hint = selected ? t('admin:userFilterSelected', { name: selected.name, id: selected.id }) : undefined
  const pendingChoice = !validEntityId(value) && !malformedEntityId(value) && !props['aria-invalid']
  const help = hint ?? (pendingChoice ? t('analytics:entityChooseHint') : undefined)
  const user = kind === 'user'

  return <div className={cn('min-w-0', className)}>
    <AutocompleteInput {...props} search optionLabelFirst value={value} displayValue={editedValue === value ? undefined : selected?.name}
      aria-describedby={[props['aria-describedby'], help ? hintId : undefined].filter(Boolean).join(' ') || undefined}
      maxLength={256} onChange={(next) => { setEditedValue(next); onChange(next) }}
      onChoose={(id) => { setEditedValue(null); setChosen(options.find((option) => String(option.id) === id) ?? null); onChoose?.(id) }}
      options={options.map((option) => ({ value: String(option.id), label: option.name, description: option.description }))}
      loading={lookup && (!ready || directory.isFetching)}
      error={ready && directory.isError ? t(user ? 'admin:userSearchUnavailable' : 'analytics:entitySearchUnavailable') : undefined}
      emptyHint={t(!allowed ? 'analytics:entityLocalOnly' : ready && !directory.isError && directory.data?.total === 0 ? user ? 'admin:userSearchEmpty' : 'analytics:entitySearchEmpty' : user ? 'admin:userSearchHelp' : 'analytics:entitySearchHelp')}
      moreHint={ready && !directory.isError && (directory.data?.total ?? 0) > (directory.data?.data.length ?? 0) ? t(user ? 'admin:userSearchMore' : 'analytics:entitySearchMore') : undefined}
      placeholder={props.placeholder ?? t(user ? 'admin:userSearchPlaceholder' : kind === 'api_key' ? 'analytics:keySearchPlaceholder' : 'analytics:channelSearchPlaceholder')}
      onFocus={(event) => { setFocused(true); onFocus?.(event) }}
      onBlur={(event) => { setFocused(false); setEditedValue(null); onBlur?.(event) }}
      onCompositionStart={(event) => { setComposing(true); props.onCompositionStart?.(event) }}
      onCompositionEnd={(event) => { setComposing(false); props.onCompositionEnd?.(event) }}
    />
    {help && <p id={hintId} className="mt-1 break-words text-xs text-muted-foreground">{help}</p>}
  </div>
}
