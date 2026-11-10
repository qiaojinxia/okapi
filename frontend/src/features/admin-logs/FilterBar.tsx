import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { SlidersHorizontal, Search } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Field } from '@/components/ui/field'
import { FilterChip, FilterChips } from '@/components/ui/filter-chip'
import { Input } from '@/components/ui/input'
import { Popover } from '@/components/ui/popover'
import { Select } from '@/components/ui/select'
import { ModelSearchInput } from '@/features/models/model-input'
import { EntitySearchInput, malformedEntityId } from '@/features/entity-search/EntitySearchInput'
import type { EntityKind, EntityOption } from '@/features/entity-search/EntitySearchInput'
import { Switch } from '@/components/ui/switch'
import { maskEmails } from '@/lib/mask'
import { invalidId, numericFilters, validRange } from './search'
import type { Draft } from './search'

const ADVANCED = ['user_id', 'api_key_id', 'channel_id', 'error_code', 'request_id', 'upstream_request_id', 'group', 'client_type', 'log_type'] as const
type Advanced = (typeof ADVANCED)[number]
const ENTITY: Record<(typeof numericFilters)[number], EntityKind> = { user_id: 'user', api_key_id: 'api_key', channel_id: 'channel' }

/// 检索条：一行——模型、时间窗、只看失败、「更多筛选」、搜索。
///
/// 「更多筛选」的九个字段放在浮层里，不占页面高度：此前它在原地展开，一展开就是三百多像素，
/// 这一页的表格是撑满视口的，筛选越展开、能看到的日志行越少。浮层里填完点「应用」或回车提交；
/// 已经生效的条件在检索条下面显示成一排小标签（「用户 alice ×」），点 × 立即去掉那一条。
export function FilterBar({
  draft,
  applied,
  onChange,
  onApply,
  onCommit,
  known,
  range,
}: {
  draft: Draft
  /// 已提交（地址栏里）的条件：标签显示的是它，不是还没点搜索的草稿。
  applied: Draft
  onChange: (d: Draft) => void
  onApply: () => void
  /// 立即提交一份条件（移除标签、清空）。
  onCommit: (d: Draft) => void
  known: Partial<Record<EntityKind, EntityOption[]>>
  /// 时间窗控件，排在模型搜索后面。
  range: React.ReactNode
}) {
  const { t } = useTranslation()
  const [open, setOpen] = useState(false)
  const [blurred, setBlurred] = useState<Partial<Record<(typeof numericFilters)[number], boolean>>>({})
  const advancedCount = ADVANCED.filter((field) => draft[field].trim()).length
  const invalid = numericFilters.some((field) => invalidId(draft[field])) || Boolean((draft.from || draft.to) && !validRange(draft))
  const label: Record<Advanced, string> = {
    user_id: t('analytics:dimUser'), api_key_id: t('analytics:dimApiKey'), channel_id: t('analytics:dimChannel'),
    error_code: t('logs:errorCode'), request_id: t('logs:requestId'), upstream_request_id: t('logs:upstreamId'),
    group: t('logs:group'), client_type: t('admin:logsClientType'), log_type: t('logs:filterType'),
  }
  const text = (field: 'error_code' | 'request_id' | 'upstream_request_id' | 'group' | 'client_type', placeholder?: string, className?: string) => (
    <Field label={label[field]} htmlFor={`lf-${field}`} className={className}>
      <Input
        id={`lf-${field}`}
        className="h-11 font-mono text-sm md:h-9 md:text-xs"
        value={draft[field]}
        placeholder={placeholder}
        onChange={(e) => onChange({ ...draft, [field]: e.target.value })}
      />
    </Field>
  )
  const entity = (field: (typeof numericFilters)[number], submit: () => void) => {
    const error = invalidId(draft[field]) && (blurred[field] || malformedEntityId(draft[field]))
    return <Field label={label[field]} htmlFor={`lf-${field}`}>
      <EntitySearchInput id={`lf-${field}`} kind={ENTITY[field]} knownOptions={known[ENTITY[field]]} value={draft[field]}
        inputClassName="h-11 md:h-9"
        aria-invalid={error || undefined} aria-describedby={error ? `lf-${field}-error` : undefined}
        onChange={(value) => { setBlurred((prev) => ({ ...prev, [field]: false })); onChange({ ...draft, [field]: value }) }}
        onBlur={() => setBlurred((prev) => ({ ...prev, [field]: true }))}
        onSubmit={() => { setBlurred((prev) => ({ ...prev, [field]: true })); submit() }} />
      {error && <span id={`lf-${field}-error`} className="text-xs text-destructive">{t(malformedEntityId(draft[field]) ? 'analytics:invalidFilterId' : 'analytics:entitySelectionRequired')}</span>}
    </Field>
  }
  // 标签文案：实体用回填的名字（「用户 alice」而非「用户 42」），查不到名字退回 #id
  const chipValue = (field: Advanced): string => {
    const value = applied[field].trim()
    if (field === 'log_type') return t(`logs:logType_${value}`)
    if (field === 'user_id' || field === 'api_key_id' || field === 'channel_id') {
      const name = known[ENTITY[field]]?.find((option) => String(option.id) === value)?.name
      // 渠道名可能就是订阅账号的邮箱：和别处一样只露两头（lib/mask）
      return name === undefined ? `#${value}` : field === 'channel_id' ? maskEmails(name) : name
    }
    return value
  }
  const chips = ADVANCED.filter((field) => applied[field].trim() !== '')
  // 这一页的规矩是「草稿 → 点搜索才查」：清空也只清草稿，和改任何一个条件一样要再点搜索
  const cleared: Draft = { ...draft, model: '', errors_only: false, ...Object.fromEntries(ADVANCED.map((field) => [field, ''])) }
  const active = advancedCount + (draft.model.trim() ? 1 : 0) + (draft.errors_only ? 1 : 0)
  return (
    <form
        data-slot="admin-log-filters"
        aria-label={t('logs:filters')}
        className="flex min-w-0 shrink-0 flex-wrap items-center gap-x-3 gap-y-2"
        onSubmit={(e) => {
          e.preventDefault()
          if (!invalid) onApply()
        }}
        onKeyDown={(event) => { if (event.key === 'Enter' && event.nativeEvent.isComposing) event.preventDefault() }}
      >
        <ModelSearchInput id="lf-model" className="w-full sm:w-56" aria-label={t('pricing:model')}
          inputClassName="h-11 md:h-9" placeholder={t('portal:logsModelHint')}
          value={draft.model} onChange={(model) => onChange({ ...draft, model })} onSubmit={() => { if (!invalid) onApply() }} />
        {range}
        {/* 后半截是一组：窗口放不下一行时整组折到第二行（两行各有其物），而不是只把最后一个按钮挤下去；
            浮层锚在这一组里的「更多筛选」下面，盖不到同一行的搜索 */}
        <div className="flex min-w-0 max-w-full flex-wrap items-center gap-x-3 gap-y-2">
        <Switch
          checked={draft.errors_only}
          onChange={(v) => onChange({ ...draft, errors_only: v })}
          label={t('logs:errorsOnly')}
        />
        <Popover
          open={open}
          onOpenChange={setOpen}
          label={t('admin:logsMoreFilters')}
          className="w-[min(44rem,calc(100vw-1rem))] p-4"
          trigger={
            <Button type="button" size="sm" variant="outline" className="min-h-11 md:min-h-9">
              <SlidersHorizontal aria-hidden className="h-3.5 w-3.5 text-muted-foreground" /><span>{t('admin:logsMoreFilters')}</span>
              {/* 计数只是视觉提示（生效的条件在下面的标签里逐条念得到），不进按钮的可访问名称 */}
              {advancedCount > 0 && <span aria-hidden data-slot="filter-count" className="rounded-full bg-primary/12 px-1.5 text-[11px] leading-[18px] font-medium text-primary tabular-nums">{advancedCount}</span>}
            </Button>
          }
        >
          {({ close }) => {
            const submit = () => {
              if (invalid) return
              onApply()
              close()
            }
            return (
              // 焦点先落在表单本身：第一个字段是带候选的检索框，一聚焦就弹候选，会盖住下面几行
              <form tabIndex={-1} data-autofocus className="flex min-w-0 flex-col gap-4 outline-none" onSubmit={(e) => { e.preventDefault(); submit() }}
                onKeyDown={(event) => { if (event.key === 'Enter' && event.nativeEvent.isComposing) event.preventDefault() }}>
                <div data-slot="admin-log-advanced" className="grid min-w-0 items-start gap-x-3 gap-y-3 sm:grid-cols-3">
                  {entity('user_id', submit)}
                  {entity('api_key_id', submit)}
                  {entity('channel_id', submit)}
                  {text('error_code', 'upstream_error')}
                  {text('request_id', 'uuid', 'sm:col-span-2')}
                  {text('group')}
                  {text('upstream_request_id', undefined, 'sm:col-span-2')}
                  {text('client_type', 'codex / claude_code / curl')}
                  <Field label={label.log_type} htmlFor="lf-log_type">
                    <Select id="lf-log_type" className="w-full" value={draft.log_type} onChange={(log_type) => onChange({ ...draft, log_type })}
                      placeholder={t('common:all')} options={[1, 2, 3, 4, 5, 6, 7].map(type => ({ value: String(type), label: t(`logs:logType_${type}`) }))} />
                  </Field>
                </div>
                <div className="flex items-center justify-between gap-2 border-t border-border/70 pt-3">
                  <Button type="button" size="sm" variant="ghost" disabled={advancedCount === 0}
                    onClick={() => onChange({ ...draft, ...Object.fromEntries(ADVANCED.map((field) => [field, ''])) })}>
                    {t('admin:logsResetMore')}
                  </Button>
                  <Button type="submit" size="sm" disabled={invalid}>{t('admin:logsApplyMore')}</Button>
                </div>
              </form>
            )
          }}
        </Popover>
          <Button type="submit" size="sm" className="min-h-11 md:min-h-9" disabled={invalid}>
            <Search className="h-3.5 w-3.5" />
            {t('common:search')}
          </Button>
          {active > 0 && (
            <Button type="button" size="sm" variant="ghost" className="-ml-1" onClick={() => onChange(cleared)}>
              {t('common:clearFilters')}
            </Button>
          )}
        </div>
      {/* 生效条件的标签跟在控件后面：这一行还有地方就接着排，没有才另起一行 */}
      {chips.length > 0 && (
        <FilterChips label={t('analytics:activeFilters')} className="flex-1 basis-64">
          {chips.map((field) => (
            <FilterChip key={field} label={label[field]} value={chipValue(field)}
              removeLabel={t('analytics:removeFilter', { name: `${label[field]} ${chipValue(field)}` })}
              onRemove={() => onCommit({ ...applied, [field]: '' })} />
          ))}
        </FilterChips>
      )}
    </form>
  )
}
