import { useNavigate } from '@tanstack/react-router'
import { Filter, X } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { AnalyticsSearch } from '@/routes/admin.stats'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Select } from '@/components/ui/select'
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

  const dimLabel: Record<FilterDim, string> = {
    user_id: t('analytics:dimUser'),
    api_key_id: t('analytics:dimApiKey'),
    channel_id: t('analytics:dimChannel'),
    model: t('analytics:dimModel'),
    group: t('analytics:dimGroup'),
  }

  const apply = (patch: Partial<AnalyticsSearch>) => {
    void navigate({ search: (prev) => cleanSearch({ ...prev, ...patch }) })
  }

  const add = () => {
    const v = value.trim()
    if (v === '') return
    if (dim === 'model' || dim === 'group') {
      apply({ [dim]: v })
    } else {
      const n = Number(v)
      if (!Number.isInteger(n) || n <= 0) return
      apply({ [dim]: n })
    }
    setValue('')
  }

  // 芯片文案：有回填名字用名字，没有退回 id / 原值
  const chips: { dim: FilterDim; text: string }[] = []
  if (search.user_id !== undefined) {
    chips.push({
      dim: 'user_id',
      text: scope?.user?.username ?? `#${search.user_id}`,
    })
  }
  if (search.api_key_id !== undefined) {
    const k = scope?.api_key
    chips.push({
      dim: 'api_key_id',
      text: k?.name ? `${k.name} (${k.key_prefix ?? ''}…)` : `#${search.api_key_id}`,
    })
  }
  if (search.channel_id !== undefined) {
    chips.push({
      dim: 'channel_id',
      text: scope?.channel?.name ?? `#${search.channel_id}`,
    })
  }
  if (search.model !== undefined) chips.push({ dim: 'model', text: search.model })
  if (search.group !== undefined) chips.push({ dim: 'group', text: search.group })

  const isNumeric = dim !== 'model' && dim !== 'group'

  return (
    <div className="flex flex-wrap items-center gap-2 rounded-lg border border-border bg-muted/30 px-3 py-2">
      <Filter className="h-4 w-4 shrink-0 text-muted-foreground" />
      <Select
        value={dim}
        onChange={(v) => setDim(v as FilterDim)}
        options={FILTER_DIMS.map((d) => ({ value: d, label: dimLabel[d] }))}
        className="w-28"
      />
      <Input
        value={value}
        inputMode={isNumeric ? 'numeric' : 'text'}
        placeholder={isNumeric ? t('analytics:filterIdPlaceholder') : t('analytics:filterTextPlaceholder')}
        className="h-9 w-44"
        onChange={(e) => setValue(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'Enter') add()
        }}
      />
      <Button size="sm" variant="outline" onClick={add} disabled={value.trim() === ''}>
        {t('analytics:addFilter')}
      </Button>

      {chips.length > 0 && <span className="mx-1 h-5 w-px bg-border" aria-hidden />}
      {chips.map((c) => (
        <span
          key={c.dim}
          className="inline-flex items-center gap-1 rounded-full bg-primary/10 py-0.5 pr-1 pl-2.5 text-xs text-primary"
        >
          <span className="text-primary/70">{dimLabel[c.dim]}</span>
          <span className="max-w-48 truncate font-medium">{c.text}</span>
          <button
            type="button"
            aria-label={t('analytics:removeFilter', { name: c.text })}
            className="rounded-full p-0.5 hover:bg-primary/15"
            onClick={() => apply({ [c.dim]: undefined })}
          >
            <X className="h-3 w-3" />
          </button>
        </span>
      ))}
      {chips.length > 1 && (
        <button
          type="button"
          className="text-xs text-muted-foreground underline-offset-2 hover:underline"
          onClick={() =>
            apply({ user_id: undefined, api_key_id: undefined, channel_id: undefined, model: undefined, group: undefined })
          }
        >
          {t('analytics:clearFilters')}
        </button>
      )}
    </div>
  )
}
