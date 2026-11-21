import { useNavigate } from '@tanstack/react-router'
import { useId, useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { AnalyticsSearch } from '@/features/analytics/route-state'
import { Button } from '@/components/ui/button'
import { FilterChip, FilterChips } from '@/components/ui/filter-chip'
import { Input } from '@/components/ui/input'
import { Select } from '@/components/ui/select'
import { ModelSearchInput } from '@/features/models/model-input'
import { EntitySearchInput, malformedEntityId, validEntityId } from '@/features/entity-search/EntitySearchInput'
import type { EntityKind, EntityOption } from '@/features/entity-search/EntitySearchInput'
import { FILTER_DIMS, cleanSearch } from '@/features/analytics/search'
import type { FilterDim } from '@/features/analytics/search'
import type { ScopeEcho } from '@/lib/cube'
import { maskEmails } from '@/lib/mask'

/// 过滤条：一个"维度 + 值"输入器 + 生效中的过滤芯片。
///
/// 不摆五个输入框：这页最常见的状态是不过滤看全站，五个空框会把注意力从数据
/// 拉到表单上。加一个过滤 = 选维度、填值、回车；芯片显示回填的名字
/// （"用户 alice"而非"用户 #42"），点 × 移除。过滤态即 URL，可分享、可回退。
export function FilterBar({ search, scope, trailing }: {
  search: AnalyticsSearch
  scope?: ScopeEcho
  /// 排在这一行末尾的控件（高级条件的入口）。
  trailing?: React.ReactNode
}) {
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
      text: maskEmails((scope?.channel?.id === search.channel_id && scope.channel.name) || `ID ${search.channel_id}`),
    })
  }
  if (search.model !== undefined) chips.push({ dim: 'model', text: search.model })
  if (search.group !== undefined) chips.push({ dim: 'group', text: search.group })

  return (
    <div
      data-slot="analytics-filters"
      className="min-w-0 rounded-xl border border-border/80 bg-card px-3.5 py-2.5 shadow-card"
    >
      <div className="flex min-w-0 flex-wrap items-start justify-between gap-x-4 gap-y-2.5">
        <section aria-label={t('analytics:quickFilters')} className="min-w-0 max-w-full flex-1 basis-96 space-y-2">
          <form className="flex min-w-0 flex-wrap items-center gap-2" onSubmit={(e) => { e.preventDefault(); add() }}>
            <Select
              aria-label={t('analytics:filterDimension')}
              value={dim}
              onChange={(v) => { setDim(v as FilterDim); changeValue('') }}
              options={FILTER_DIMS.map((d) => ({ value: d, label: dimLabel[d] }))}
              className="w-24 shrink-0 sm:w-28"
            />
            {dim === 'model' ? <ModelSearchInput
              id={inputId} aria-label={dimLabel[dim]} value={value} onChange={changeValue}
              placeholder={t('analytics:filterTextPlaceholder')} className="min-w-36 flex-1 sm:max-w-80" onSubmit={add}
            /> : dim === 'group' ? <Input
              id={inputId}
              aria-label={dimLabel[dim]}
              value={value}
              placeholder={t('analytics:filterTextPlaceholder')}
              className="h-9 min-w-36 flex-1 sm:max-w-80"
              onChange={(e) => changeValue(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter' && e.nativeEvent.isComposing) e.preventDefault()
              }}
            /> : <EntitySearchInput id={inputId} kind={kinds[dim]} knownOptions={known[kinds[dim]]}
              aria-label={dimLabel[dim]} value={value} onChange={changeValue} onSubmit={add}
              onBlur={() => setBlurred(true)} className="min-w-36 flex-1 sm:max-w-80"
              aria-invalid={showError || undefined} aria-describedby={showError ? `${inputId}-error` : undefined} />}
            <Button type="submit" size="sm" variant="outline" className="min-h-9 shrink-0 max-sm:w-full" disabled={value.trim() === '' || numericInvalid}>
              {t('analytics:addFilter')}
            </Button>
          </form>
          {showError && <p id={`${inputId}-error`} role="alert" className="text-xs text-destructive">{t(malformedEntityId(value) ? 'analytics:invalidFilterId' : 'analytics:entitySelectionRequired')}</p>}
          {chips.length > 0 && (
            <div className="flex flex-wrap items-center gap-2 pt-0.5">
              <span className="text-[11px] font-medium text-muted-foreground">{t('analytics:activeFilters')}</span>
              <FilterChips label={t('analytics:activeFilters')}>
                {chips.map((c) => (
                  <FilterChip key={c.dim} label={dimLabel[c.dim]} value={c.text}
                    removeLabel={t('analytics:removeFilter', { name: c.text })} onRemove={() => apply({ [c.dim]: undefined })} />
                ))}
                {chips.length > 1 && (
                  <button
                    type="button"
                    className="min-h-7 cursor-pointer rounded px-2 text-xs text-muted-foreground underline-offset-2 outline-none transition-colors hover:text-foreground hover:underline focus-visible:ring-2 focus-visible:ring-primary/40"
                    onClick={() =>
                      apply({ user_id: undefined, api_key_id: undefined, channel_id: undefined, model: undefined, group: undefined })
                    }
                  >
                    {t('analytics:clearFilters')}
                  </button>
                )}
              </FilterChips>
            </div>
          )}
        </section>
        {trailing && (
          <div className="flex min-w-0 max-w-full shrink-0 items-center lg:border-l lg:border-border/65 lg:pl-4">
            {trailing}
          </div>
        )}
      </div>
    </div>
  )
}
