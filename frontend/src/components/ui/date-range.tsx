import { CalendarDays, ChevronDown } from 'lucide-react'
import { useState } from 'react'
import type { ButtonHTMLAttributes } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from './button'
import { Calendar, dayCount, isDay } from './calendar'
import { Popover } from './popover'
import { cn } from '@/lib/utils'

export interface DateRange { start: string; end: string }

const EARLIEST = '1970-01-01'
const MAX_DAYS = 366

/// 范围合法：两端是真实日期、不晚于 today、不早于 1970、终点不早于起点、不超过 366 天。
export function validRange(range: DateRange, today: string): boolean {
  const { start, end } = range
  if (!isDay(start) || !isDay(end)) return false
  return end <= today && start >= EARLIEST && end >= start && dayCount(start, end) <= MAX_DAYS
}

const TRIGGER =
  'inline-flex h-9 min-w-0 max-w-full items-center gap-2 rounded-lg border border-border bg-card px-3 text-sm outline-none transition-colors hover:bg-accent/60 focus-visible:ring-2 focus-visible:ring-primary/40 aria-expanded:border-primary/50'

/// 日期弹出框的触发按钮：日历图标 + 一句文字（没选时是「自定义日期」，选了是起止）。
/// `active` = 已经按自定义日期在过滤，按钮淡淡染一层主色提示。
export function DateTrigger({ active = false, className, children, ...props }: ButtonHTMLAttributes<HTMLButtonElement> & { active?: boolean }) {
  return (
    <button type="button" {...props} className={cn(TRIGGER, active && 'border-primary/40 bg-primary/5', className)}>
      <CalendarDays aria-hidden className="size-4 shrink-0 text-muted-foreground" />
      <span className="truncate">{children}</span>
    </button>
  )
}

/// 弹出框里的内容：起止两个日期输入 + 月历范围点选，应用 / 取消。
/// 点选的顺序：第一下落起点（并清掉终点），第二下落终点；点在起点之前就重新当起点。
/// 输入框与月历是同一份草稿的两种写法——敲输入框月历翻到那一天，点月历输入框跟着变。
function RangePanel({ today, draft, onDraft, onSubmit, onCancel, submitLabel, onClear }: {
  today: string
  draft: DateRange
  onDraft: (range: DateRange) => void
  onSubmit: () => void
  onCancel: () => void
  submitLabel: string
  /// 给了才显示「清除」：把范围清空（表单里的可选日期范围用）。
  onClear?: () => void
}) {
  const { t } = useTranslation()
  const [phase, setPhase] = useState<'start' | 'end'>('start')
  const valid = validRange(draft, today)
  const pick = (day: string) => {
    if (phase === 'end' && isDay(draft.start) && day >= draft.start) {
      onDraft({ start: draft.start, end: day })
      setPhase('start')
    } else {
      onDraft({ start: day, end: '' })
      setPhase('end')
    }
  }
  const type = (field: keyof DateRange, value: string) => {
    onDraft({ ...draft, [field]: value })
    setPhase('start')
  }
  return (
    <form
      className="grid w-[17.5rem] gap-3"
      onSubmit={(event) => {
        event.preventDefault()
        if (valid) onSubmit()
      }}
    >
      <div className="grid grid-cols-2 gap-2">
        {(['start', 'end'] as const).map((field) => (
          <label key={field} className="grid min-w-0 gap-1 text-xs text-muted-foreground">
            {t(`charts:range_${field}`)}
            <input
              type="date"
              value={draft[field]}
              min={EARLIEST}
              max={today}
              onChange={(event) => type(field, event.target.value)}
              // 日历图标藏掉：弹出框里已经有月历，不要再从输入框里弹出第二层原生日历
              className="h-9 w-full min-w-0 rounded-md border border-border bg-card px-2 text-sm text-foreground outline-none focus-visible:border-primary focus-visible:ring-2 focus-visible:ring-primary/25 dark:[color-scheme:dark] [&::-webkit-calendar-picker-indicator]:hidden"
            />
          </label>
        ))}
      </div>
      <Calendar today={today} mode="range" start={draft.start} end={draft.end} onPick={pick} />
      <p className="text-xs leading-5 text-muted-foreground">{t('charts:rangeHint')}</p>
      {/* 窗口放不下整块浮层时浮层自己滚动；操作按钮钉在底部（-bottom-3 抵掉浮层的内边距，内容不会从缝里透出来），不用先滚到底才找得到「应用」 */}
      <div className="sticky -bottom-3 z-10 -mx-3 -mb-3 flex items-center justify-end gap-2 border-t border-border bg-popover px-3 py-2.5">
        {onClear !== undefined && (
          <Button type="button" variant="ghost" size="sm" className="mr-auto text-muted-foreground" onClick={onClear}>
            {t('common:clear')}
          </Button>
        )}
        <Button type="button" variant="outline" size="sm" onClick={onCancel}>{t('common:cancel')}</Button>
        <Button type="submit" size="sm" disabled={!valid}>{submitLabel}</Button>
      </div>
    </form>
  )
}

/// 自定义日期：页面上只有一个触发按钮（没选时写「自定义日期」，选了写起止日期），
/// 点开是弹出框，应用才生效——不再把日期输入展开在页面里挤开版面。
export function DateRangePicker({ today, value, onApply, className }: {
  today: string
  value: DateRange | null
  onApply: (range: DateRange) => void
  className?: string
}) {
  const { t } = useTranslation()
  const [open, setOpen] = useState(false)
  const start = value?.start ?? today
  const end = value?.end ?? today
  const [draft, setDraft] = useState<DateRange>({ start, end })
  // 每次打开都从当前已应用的范围开始：上次没应用的草稿、以及地址前进 / 后退带来的变化都不残留
  const change = (next: boolean) => {
    if (next) setDraft({ start, end })
    setOpen(next)
  }
  return (
    <Popover
      open={open}
      onOpenChange={change}
      label={t('charts:customRange')}
      trigger={<DateTrigger active={value !== null} className={className}>{value ? `${value.start} — ${value.end}` : t('charts:customRange')}</DateTrigger>}
    >
      {({ close }) => (
        <RangePanel
          today={today}
          draft={draft}
          onDraft={setDraft}
          submitLabel={t('charts:applyRange')}
          onCancel={close}
          onSubmit={() => {
            onApply(draft)
            close()
          }}
        />
      )}
    </Popover>
  )
}

/// 表单里的日期范围字段：像输入框一样占满一格，点开弹出框选好点「确定」写回表单草稿
/// （表单自己再统一应用）。两端都空 = 不指定日期，由表单的其他条件决定窗口。
export function DateRangeField({ today, value, onChange, placeholder, label, id, className }: {
  today: string
  value: { start: string; end: string }
  onChange: (range: DateRange) => void
  /// 没选日期时按钮上写什么。
  placeholder: string
  /// 弹出框的可访问名称。
  label: string
  id?: string
  className?: string
}) {
  const { t } = useTranslation()
  const [open, setOpen] = useState(false)
  const [draft, setDraft] = useState<DateRange>(value)
  const text = value.start !== '' && value.end !== '' ? `${value.start} — ${value.end}` : value.start !== '' ? `${value.start} —` : value.end !== '' ? `— ${value.end}` : ''
  const change = (next: boolean) => {
    if (next) setDraft(value)
    setOpen(next)
  }
  return (
    <Popover
      open={open}
      onOpenChange={change}
      label={label}
      trigger={
        <button id={id} type="button" aria-label={`${label} ${text === '' ? placeholder : text}`} className={cn(TRIGGER, 'w-full justify-between', className)}>
          <span className="flex min-w-0 items-center gap-2">
            <CalendarDays aria-hidden className="size-4 shrink-0 text-muted-foreground" />
            <span className={cn('truncate', text === '' && 'text-muted-foreground')}>{text === '' ? placeholder : text}</span>
          </span>
          <ChevronDown aria-hidden className="size-4 shrink-0 text-muted-foreground" />
        </button>
      }
    >
      {({ close }) => (
        <RangePanel
          today={today}
          draft={draft}
          onDraft={setDraft}
          submitLabel={t('charts:rangeConfirm')}
          onCancel={close}
          onClear={text === '' ? undefined : () => {
            onChange({ start: '', end: '' })
            close()
          }}
          onSubmit={() => {
            onChange(draft)
            close()
          }}
        />
      )}
    </Popover>
  )
}
