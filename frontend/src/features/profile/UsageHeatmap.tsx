import { useMemo, useRef } from 'react'
import { useTranslation } from 'react-i18next'
import { Tooltip } from '@/components/ui/tooltip'
import { formatMoney } from '@/lib/money'
import { cn } from '@/lib/utils'
import { calendarDate, heatColors, intensity, intensityThresholds } from './activity'
import type { ActivityDay, Metric } from './activity'

export function UsageHeatmap({ days, today, metric, selected, onSelect }: {
  days: ActivityDay[]
  today: string
  metric: Metric
  selected: string
  onSelect: (day: string) => void
}) {
  const { t, i18n } = useTranslation()
  const buttons = useRef(new Map<string, HTMLButtonElement>())
  const thresholds = useMemo(() => intensityThresholds(days, metric), [days, metric])
  const offset = calendarDate(days[0].day).getUTCDay()
  const weeks = Math.ceil((offset + days.length) / 7)
  const lastAvailable = days.findLastIndex((day) => day.day <= today)
  const monthFormat = new Intl.DateTimeFormat(i18n.language, { month: 'short', timeZone: 'UTC' })
  const dayFormat = new Intl.DateTimeFormat(i18n.language, { weekday: 'short', timeZone: 'UTC' })
  const monthLabels = days.flatMap((day, index) => day.day.endsWith('-01')
    ? [{ month: monthFormat.format(calendarDate(day.day)), week: Math.floor((offset + index) / 7) }]
    : [])
  const move = (index: number, key: string) => {
    const next = key === 'ArrowRight' ? index + 7 : key === 'ArrowLeft' ? index - 7
      : key === 'ArrowDown' ? index + 1 : key === 'ArrowUp' ? index - 1
        : key === 'Home' ? 0 : key === 'End' ? lastAvailable : undefined
    if (next === undefined) return false
    const target = days[Math.max(0, Math.min(lastAvailable, next))]
    const button = buttons.current.get(target.day)
    button?.focus({ preventScroll: true })
    button?.scrollIntoView({ block: 'nearest', inline: 'nearest' })
    return true
  }

  return (
    <div className="space-y-3">
      <div className="overflow-x-auto pb-2" role="group" aria-label={t('profile:heatmap')} aria-describedby="heatmap-help">
        <div className="grid w-max gap-x-2 [--heat-cell:24px] sm:[--heat-cell:13px]" style={{ gridTemplateColumns: 'auto 1fr' }}>
          <div />
          <div className="mb-2 grid h-5 gap-[3px] text-xs text-muted-foreground" style={{ gridTemplateColumns: `repeat(${weeks}, var(--heat-cell))` }} aria-hidden>
            {monthLabels.map(({ month, week }) => <span key={month} className="min-w-0 whitespace-nowrap" style={{ gridColumn: week + 1 }}>{month}</span>)}
          </div>
          <div className="grid grid-rows-7 gap-[3px] pr-1 text-[10px] text-muted-foreground" aria-hidden>
            {Array.from({ length: 7 }, (_, day) => (
              <span key={day} className="flex h-[var(--heat-cell)] items-center">{day % 2 === 1 ? dayFormat.format(new Date(Date.UTC(2023, 0, day + 1))) : ''}</span>
            ))}
          </div>
          <div className="grid grid-flow-col grid-rows-7 gap-[3px]" style={{ gridTemplateColumns: `repeat(${weeks}, var(--heat-cell))` }}>
            {Array.from({ length: offset }, (_, i) => <span key={`pad-${i}`} aria-hidden />)}
            {days.map((day, index) => {
              const future = day.day > today
              const label = `${day.day} · ${day.tokens.toLocaleString(i18n.language)} Token · ${t('profile:requestCount', { count: day.requests })} · ${formatMoney(day.amount_micro, i18n.language)}`
              return future ? <span key={day.day} aria-hidden data-future={day.day} className="size-[var(--heat-cell)] rounded-[3px] border border-dashed border-border/50" /> : (
                <Tooltip key={day.day} content={label} className="min-w-0">
                  <button
                    type="button"
                    ref={(node) => { if (node) buttons.current.set(day.day, node); else buttons.current.delete(day.day) }}
                    data-day={day.day}
                    data-level={intensity(day[metric], thresholds)}
                    aria-label={label}
                    aria-pressed={selected === day.day}
                    tabIndex={selected === day.day ? 0 : -1}
                    onClick={() => onSelect(day.day)}
                    onFocus={() => onSelect(day.day)}
                    onKeyDown={(event) => { if (move(index, event.key)) event.preventDefault() }}
                    className={cn('size-[var(--heat-cell)] shrink-0 cursor-pointer rounded-[3px] border transition-[box-shadow] outline-none hover:ring-2 hover:ring-foreground/50',
                      heatColors[intensity(day[metric], thresholds)],
                      selected === day.day && 'ring-2 ring-foreground ring-offset-1 ring-offset-card',
                      'focus-visible:ring-2 focus-visible:ring-primary focus-visible:ring-offset-2')}
                  />
                </Tooltip>
              )
            })}
          </div>
        </div>
      </div>
      <div className="flex flex-wrap items-center justify-between gap-3 text-xs text-muted-foreground">
        <p id="heatmap-help" className="max-w-lg leading-5">{t('profile:heatmapHelp')}</p>
        <div className="flex shrink-0 items-center gap-1.5" aria-label={t('profile:legend')}>
          <span className="mr-1">{t('profile:less')}</span>
          {heatColors.map((color) => <span key={color} className={cn('h-3 w-3 rounded-[3px] border', color)} aria-hidden />)}
          <span className="ml-1">{t('profile:more')}</span>
        </div>
      </div>
    </div>
  )
}
