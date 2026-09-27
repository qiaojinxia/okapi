import { useNavigate } from '@tanstack/react-router'
import { Filter, X } from 'lucide-react'
import { useId, useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { AnalyticsSearch } from '@/routes/admin.stats'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Select } from '@/components/ui/select'
import { ModelSearchInput } from '@/features/models/model-input'
import { EntitySearchInput, malformedEntityId, validEntityId } from '@/features/entity-search/EntitySearchInput'
import type { EntityKind, EntityOption } from '@/features/entity-search/EntitySearchInput'
import { FILTER_DIMS, cleanSearch } from '@/features/analytics/search'
import type { FilterDim } from '@/features/analytics/search'
import type { ScopeEcho } from '@/features/analytics/types'

/// 过滤条：一个"维度 + 值"输入器 + 生效中的过滤芯片。
///
/// 不摆五个输入框：这页最常见的状态是不过滤看全站，五个空框会把注意力从数据
/// 拉到表单上。加一个过滤 = 选维度、填值、回车；芯片显示回填的名字
/// （"用户 alice"而非"用户 #42"），点 × 移除。过滤态即 URL，可分享、可回退。
export function FilterBar({ search, scope }: { search: AnalyticsSearch; scope?: ScopeEcho }) {
  const { t } = useTranslation()
  const navigate = useNavigate({ from: '/admin/stats' })
  const [dim, setDim] = useState<FilterDim>('model')
  const [value, setValue] = useState('')
  const [blurred, setBlurred] = useState(false)
  const inputId = useId()
  const isNumeric = dim !== 'model' && dim !== 'group'
  const numericInvalid = isNumeric && !validEntityId(value)
  const showError = numericInvalid && (blurred || malformedEntityId(value))
  const changeValue = (next: string) => { setValue(next); setBlurred(false) }
  const kinds: Record<'user_id' | 'api_key_id' | 'channel_id', EntityKind> = { user_id: 'user', api_key_id: 'api_key', channel_id: 'channel' }
  const known: Record<EntityKind, EntityOption[]> = {
    user: scope?.user?.username ? [{ id: scope.user.id, name: scope.user.username }] : [],
    api_key: scope?.api_key?.name ? [{ id: scope.api_key.id, name: scope.api_key.name, description: scope.api_key.key_prefix ?? undefined }] : [],
    channel: scope?.channel?.name ? [{ id: scope.channel.id, name: scope.channel.name, description: scope.channel.provider ?? undefined }] : [],
  }

  const dimLabel: Record<FilterDim, string> = {
    user_id: t('analytics:dimUser'),
    api_key_id: t('analytics:dimApiKey'),
    channel_id: t('analytics:dimChannel'),
    model: t('analytics:dimModel'),
    group: t('analytics:dimGroup'),
  }

  const apply = (patch: Partial<AnalyticsSearch>) => {
    void navigate({ resetScroll: false, search: (prev) => cleanSearch({ ...prev, ...patch }) })
  }

  const add = () => {
    const v = value.trim()
    if (v === '' || numericInvalid) return
    if (dim === 'model' || dim === 'group') {
      // 单项聚焦与多选比较互斥，避免旧列表继续叠加导致查不到数据。
      apply({ [dim]: v, [dim === 'model' ? 'models' : 'groups']: undefined })
    } else {
      const n = Number(v)
      apply({ [dim]: n })
    }
    setValue('')
  }

  // 芯片文案：有回填名字用名字，没有退回 id / 原值
  const chips: { dim: FilterDim; text: string }[] = []
  if (search.user_id !== undefined) {
    chips.push({
      dim: 'user_id',
      text: (scope?.user?.id === search.user_id && scope.user.username) || `ID ${search.user_id}`,
    })
  }
  if (search.api_key_id !== undefined) {
    const k = scope?.api_key?.id === search.api_key_id ? scope.api_key : undefined
    chips.push({
      dim: 'api_key_id',
      text: k?.name ? `${k.name}${k.key_prefix ? ` (${k.key_prefix}…)` : ''}` : `ID ${search.api_key_id}`,
    })
  }
  if (search.channel_id !== undefined) {
    chips.push({
      dim: 'channel_id',
      text: (scope?.channel?.id === search.channel_id && scope.channel.name) || `ID ${search.channel_id}`,
    })
  }
  if (search.model !== undefined) chips.push({ dim: 'model', text: search.model })
  if (search.group !== undefined) chips.push({ dim: 'group', text: search.group })

  return (
    <section aria-label={t('analytics:quickFilters')} className="min-w-0 space-y-2 rounded-xl border border-border bg-card px-3 py-2.5">
      <form className="flex min-w-0 flex-wrap items-start gap-2" onSubmit={(e) => { e.preventDefault(); add() }}>
      <Filter aria-hidden className="mt-2.5 hidden h-4 w-4 shrink-0 text-muted-foreground sm:block" />
      <Select
        aria-label={t('analytics:filterDimension')}
        value={dim}
        onChange={(v) => { setDim(v as FilterDim); changeValue('') }}
        options={FILTER_DIMS.map((d) => ({ value: d, label: dimLabel[d] }))}
        className="w-24 shrink-0 sm:w-28"
      />
      {dim === 'model' ? <ModelSearchInput
        id={inputId} aria-label={dimLabel[dim]} value={value} onChange={changeValue}
        placeholder={t('analytics:filterTextPlaceholder')} className="min-w-36 flex-1 sm:max-w-96" onSubmit={add}
      /> : dim === 'group' ? <Input
        id={inputId}
        aria-label={dimLabel[dim]}
        value={value}
        placeholder={t('analytics:filterTextPlaceholder')}
        className="h-9 min-w-36 flex-1 sm:max-w-96"
        onChange={(e) => changeValue(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'Enter' && e.nativeEvent.isComposing) e.preventDefault()
        }}
      /> : <EntitySearchInput id={inputId} kind={kinds[dim]} knownOptions={known[kinds[dim]]}
        aria-label={dimLabel[dim]} value={value} onChange={changeValue} onSubmit={add}
        onBlur={() => setBlurred(true)} className="min-w-36 flex-1 sm:max-w-96"
        aria-invalid={showError || undefined} aria-describedby={showError ? `${inputId}-error` : undefined} />}
      <Button type="submit" size="sm" variant="outline" className="min-h-9 max-sm:w-full" disabled={value.trim() === '' || numericInvalid}>
        {t('analytics:addFilter')}
      </Button>
      </form>
      {showError && <p id={`${inputId}-error`} role="alert" className="text-xs text-destructive">{t(malformedEntityId(value) ? 'analytics:invalidFilterId' : 'analytics:entitySelectionRequired')}</p>}

      {chips.length > 0 && <div className="flex min-w-0 flex-wrap items-center gap-2 border-t border-border pt-2" aria-label={t('analytics:activeFilters')}>
      <span className="text-xs text-muted-foreground">{t('analytics:activeFilters')}</span>
      {chips.map((c) => (
        <span
          key={c.dim}
          className="inline-flex max-w-full items-center gap-1 rounded-full bg-primary/10 py-0.5 pr-1 pl-2.5 text-xs text-primary"
        >
          <span className="shrink-0 text-primary/70">{dimLabel[c.dim]}</span>
          <span className="min-w-0 max-w-64 truncate font-medium" title={c.text}>{c.text}</span>
          <button
            type="button"
            aria-label={t('analytics:removeFilter', { name: c.text })}
            className="flex h-7 w-7 shrink-0 items-center justify-center rounded-full outline-none hover:bg-primary/15 focus-visible:ring-2 focus-visible:ring-primary/40"
            onClick={() => apply({ [c.dim]: undefined })}
          >
            <X className="h-3 w-3" />
          </button>
        </span>
      ))}
      {chips.length > 1 && (
        <button
          type="button"
          className="min-h-8 rounded px-2 text-xs text-muted-foreground underline-offset-2 outline-none hover:underline focus-visible:ring-2 focus-visible:ring-primary/40"
          onClick={() =>
            apply({ user_id: undefined, api_key_id: undefined, channel_id: undefined, model: undefined, group: undefined })
          }
        >
          {t('analytics:clearFilters')}
        </button>
      )}
      </div>}
    </section>
  )
}
