import { useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { CalendarDays } from 'lucide-react'
import { Button } from './button'
import { cn } from '@/lib/utils'

export interface DateRange { start: string; end: string }

export function DateRangePicker({ today, value, onApply, className, closeOnApply = false }: { today: string; value: DateRange | null; onApply: (range: DateRange) => void; className?: string; closeOnApply?: boolean }) {
  const { t } = useTranslation()
  const details = useRef<HTMLDetailsElement>(null)
  const [draft, setDraft] = useState<DateRange>(value ?? { start: today, end: today })
  const start = value?.start ?? today, end = value?.end ?? today
  // 地址前进/后退或切回预设日期时，输入框同步到当前已应用的范围。
  useEffect(() => { setDraft({ start, end }) }, [start, end])
  const length = (new Date(`${draft.end}T00:00:00Z`).getTime() - new Date(`${draft.start}T00:00:00Z`).getTime()) / 86400_000 + 1
  const valid = !!draft.start && !!draft.end && draft.end <= today && draft.start >= '1970-01-01' && length > 0 && length <= 366
  return <details ref={details} className={cn('w-full self-start rounded-lg border border-border bg-card px-3 py-2 text-sm sm:w-auto open:w-full', className)}>
    <summary className="flex min-h-7 cursor-pointer items-center gap-2 outline-none focus-visible:ring-2 focus-visible:ring-primary/40"><CalendarDays className="h-4 w-4 text-muted-foreground" />{value ? `${value.start} — ${value.end}` : t('charts:customRange')}</summary>
    <form onSubmit={(e) => { e.preventDefault(); if (valid) { onApply(draft); if (closeOnApply && details.current) { details.current.open = false; details.current.querySelector('summary')?.focus() } } }} className="mt-3 flex flex-wrap items-end gap-3 border-t border-border pt-3">
      {(['start', 'end'] as const).map((field) => <label key={field} className="grid gap-1 text-xs text-muted-foreground">{t(`charts:range_${field}`)}<input type="date" required value={draft[field]} min="1970-01-01" max={today} onChange={(e) => setDraft({ ...draft, [field]: e.target.value })} className="h-10 rounded-md border border-border bg-background px-2 text-sm text-foreground dark:[color-scheme:dark]" /></label>)}
      <Button type="submit" disabled={!valid}>{t('charts:applyRange')}</Button>
      <p className="w-full text-xs text-muted-foreground">{t('charts:rangeHint')}</p>
    </form>
  </details>
}
