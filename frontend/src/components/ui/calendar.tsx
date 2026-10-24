import { ChevronLeft, ChevronRight, ChevronsLeft, ChevronsRight } from 'lucide-react'
import { useEffect, useLayoutEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { cn } from '@/lib/utils'

// 日期一律用 'YYYY-MM-DD' 字符串，计算全走 UTC：日历只关心"哪一天"，不该被浏览器时区拐走一天。
// 字符串按字典序就是时间序，比较大小直接用 < / >。
const DAY = /^(\d{4})-(\d{2})-(\d{2})$/
const DAY_MS = 86_400_000

export function isDay(value: string): boolean {
  const m = DAY.exec(value)
  if (m === null) return false
  const [y, mo, d] = [Number(m[1]), Number(m[2]), Number(m[3])]
  const date = new Date(Date.UTC(y, mo - 1, d))
  return date.getUTCFullYear() === y && date.getUTCMonth() === mo - 1 && date.getUTCDate() === d
}

const toUtc = (day: string) => {
  const [y, m, d] = day.split('-').map(Number)
  return Date.UTC(y, m - 1, d)
}
const fromUtc = (ms: number) => new Date(ms).toISOString().slice(0, 10)
export const addDays = (day: string, n: number) => fromUtc(toUtc(day) + n * DAY_MS)
/// 起止两端都算的天数（同一天 = 1）。
export const dayCount = (start: string, end: string) => (toUtc(end) - toUtc(start)) / DAY_MS + 1
/// 加减整月；目标月没有这一天（31 号 +1 月）就取月末。
export function addMonths(day: string, n: number): string {
  const [y, m, d] = day.split('-').map(Number)
  const first = new Date(Date.UTC(y, m - 1 + n, 1))
  const last = new Date(Date.UTC(first.getUTCFullYear(), first.getUTCMonth() + 1, 0)).getUTCDate()
  return fromUtc(Date.UTC(first.getUTCFullYear(), first.getUTCMonth(), Math.min(d, last)))
}
const weekday = (day: string) => new Date(toUtc(day)).getUTCDay() // 0 = 周日
const clamp = (day: string, lo: string, hi: string) => (day < lo ? lo : day > hi ? hi : day)

interface CalendarProps {
  /// 今天（按调用方关心的时区算好的 'YYYY-MM-DD'）：画一圈标记，也是缺省的最晚可选日。
  today: string
  /// `range`：起止两端依次点选；`single`：只用 `start`。
  mode?: 'single' | 'range'
  start?: string
  end?: string
  min?: string
  /// 最晚可选日，缺省 = today（不能选未来）。
  max?: string
  /// 范围内个别不可选的日子（个人中心只有有数据的日子能点）。
  isDisabled?: (day: string) => boolean
  /// 打开时把焦点直接送到当前选中 / 漫游的那一天（弹出框里没有输入框可先聚焦时用），方向键立刻可用。
  autoFocus?: boolean
  onPick: (day: string) => void
  className?: string
}

/// 月历（无依赖）：一个月的网格，范围选择时起止之间连成一条淡色带，悬停预览终点。
///
/// 键盘：方向键移动（← → 一天、↑ ↓ 一周），Home / End 到周首 / 周尾，PageUp / PageDown 翻月
/// （加 Shift 翻年），Enter / 空格选择；只有当前的"漫游"日在 Tab 序里。
/// 周起点跟语言走：中文周一起，其余周日起。
export function Calendar({ today, mode = 'range', start = '', end = '', min = '1970-01-01', max, isDisabled, autoFocus = false, onPick, className }: CalendarProps) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const zh = locale.startsWith('zh')
  const weekStart = zh ? 1 : 0
  const lo = min
  const hi = max ?? today
  const anchorDay = isDay(start) ? start : isDay(end) ? end : today
  const [month, setMonth] = useState(clamp(anchorDay, lo, hi).slice(0, 7))
  const [focusDay, setFocusDay] = useState(clamp(anchorDay, lo, hi))
  const [hover, setHover] = useState('')
  const grid = useRef<HTMLTableElement>(null)
  const refocus = useRef(false)

  // 起点被外面改了（输入框里敲了另一个月的日期）：月历翻到那个月。
  // 只在起点变化时对齐——用户自己翻月不该被拉回来，所以依赖里没有 month。
  useEffect(() => {
    if (isDay(start)) setMonth(start.slice(0, 7))
  }, [start])

  // 键盘移动后把焦点送到新的那一天（月份可能刚换，等渲染完再聚焦）
  useLayoutEffect(() => {
    if (!refocus.current) return
    refocus.current = false
    grid.current?.querySelector<HTMLButtonElement>(`[data-date="${focusDay}"]`)?.focus({ preventScroll: true })
  })

  const monthStart = `${month}-01`
  const total = new Date(Date.UTC(Number(month.slice(0, 4)), Number(month.slice(5, 7)), 0)).getUTCDate()
  const lead = (weekday(monthStart) - weekStart + 7) % 7
  const cells: (string | null)[] = [
    ...Array.from({ length: lead }, () => null),
    ...Array.from({ length: total }, (_, i) => `${month}-${String(i + 1).padStart(2, '0')}`),
  ]
  while (cells.length % 7 !== 0) cells.push(null)
  const weeks = Array.from({ length: cells.length / 7 }, (_, i) => cells.slice(i * 7, i * 7 + 7))

  const outside = (day: string) => day < lo || day > hi
  const roving = focusDay.slice(0, 7) === month ? focusDay : (cells.find((d): d is string => d !== null && !outside(d)) ?? monthStart)

  const titleFormat = new Intl.DateTimeFormat(locale, { year: 'numeric', month: 'long', timeZone: 'UTC' })
  const dayFormat = new Intl.DateTimeFormat(locale, { dateStyle: 'full', timeZone: 'UTC' })
  const shortName = new Intl.DateTimeFormat(locale, { weekday: zh ? 'narrow' : 'short', timeZone: 'UTC' })
  const longName = new Intl.DateTimeFormat(locale, { weekday: 'long', timeZone: 'UTC' })
  // 2024-01-07 是周日
  const weekdayHeads = Array.from({ length: 7 }, (_, i) => {
    const date = new Date(Date.UTC(2024, 0, 7 + ((weekStart + i) % 7)))
    return { short: shortName.format(date), long: longName.format(date) }
  })

  const showMonth = (next: string) => setMonth(next.slice(0, 7))
  const canPrev = (months: number) => addMonths(monthStart, -months).slice(0, 7) >= lo.slice(0, 7)
  const canNext = (months: number) => addMonths(monthStart, months).slice(0, 7) <= hi.slice(0, 7)

  const move = (to: string) => {
    const next = clamp(to, lo, hi)
    setFocusDay(next)
    showMonth(next)
    refocus.current = true
  }
  const onKeyDown = (event: React.KeyboardEvent<HTMLTableElement>) => {
    const day = (event.target as HTMLElement).getAttribute('data-date')
    if (day === null || event.altKey || event.ctrlKey || event.metaKey) return
    const column = (weekday(day) - weekStart + 7) % 7
    const target: Record<string, string> = {
      ArrowLeft: addDays(day, -1),
      ArrowRight: addDays(day, 1),
      ArrowUp: addDays(day, -7),
      ArrowDown: addDays(day, 7),
      Home: addDays(day, -column),
      End: addDays(day, 6 - column),
      PageUp: addMonths(day, event.shiftKey ? -12 : -1),
      PageDown: addMonths(day, event.shiftKey ? 12 : 1),
    }
    if (!(event.key in target)) return
    event.preventDefault()
    move(target[event.key])
  }

  // 悬停预览：只选了起点时，起点与悬停日之间也连成带，终点位置一目了然
  const preview = mode === 'range' && isDay(start) && !isDay(end) && hover !== '' ? hover : ''
  const bandLo = mode === 'range' && isDay(start) ? (isDay(end) ? start : preview !== '' && preview < start ? preview : start) : ''
  const bandHi = mode === 'range' && isDay(start) ? (isDay(end) ? end : preview !== '' && preview > start ? preview : start) : ''

  const nav = 'inline-flex size-8 items-center justify-center rounded-md text-muted-foreground outline-none transition-colors hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-primary/50 disabled:pointer-events-none disabled:opacity-35'
  return (
    <div data-slot="calendar" className={cn('w-[17.5rem] select-none', className)}>
      <div className="mb-2 flex items-center justify-between gap-1">
        <div className="flex">
          <button type="button" className={nav} disabled={!canPrev(12)} aria-label={t('charts:prevYear')} onClick={() => showMonth(addMonths(monthStart, -12))}>
            <ChevronsLeft aria-hidden className="size-4" />
          </button>
          <button type="button" className={nav} disabled={!canPrev(1)} aria-label={t('charts:prevMonth')} onClick={() => showMonth(addMonths(monthStart, -1))}>
            <ChevronLeft aria-hidden className="size-4" />
          </button>
        </div>
        <div className="text-sm font-medium" aria-live="polite">{titleFormat.format(new Date(toUtc(monthStart)))}</div>
        <div className="flex">
          <button type="button" className={nav} disabled={!canNext(1)} aria-label={t('charts:nextMonth')} onClick={() => showMonth(addMonths(monthStart, 1))}>
            <ChevronRight aria-hidden className="size-4" />
          </button>
          <button type="button" className={nav} disabled={!canNext(12)} aria-label={t('charts:nextYear')} onClick={() => showMonth(addMonths(monthStart, 12))}>
            <ChevronsRight aria-hidden className="size-4" />
          </button>
        </div>
      </div>
      <table ref={grid} role="grid" data-month={month} aria-label={titleFormat.format(new Date(toUtc(monthStart)))} className="w-full table-fixed border-collapse" onKeyDown={onKeyDown} onMouseLeave={() => setHover('')}>
        <thead>
          <tr>
            {weekdayHeads.map((head) => (
              <th key={head.long} scope="col" abbr={head.long} className="h-8 text-xs font-normal text-muted-foreground">{head.short}</th>
            ))}
          </tr>
        </thead>
        <tbody>
          {weeks.map((week, row) => (
            <tr key={row}>
              {week.map((day, column) => {
                if (day === null) return <td key={column} />
                const isStart = day === start
                const isEnd = mode === 'range' && day === end
                const edge = isStart || isEnd
                const inBand = bandLo !== '' && day >= bandLo && day <= bandHi
                const blocked = outside(day) || isDisabled?.(day) === true
                return (
                  <td
                    key={day}
                    role="gridcell"
                    aria-selected={edge || (inBand && day !== bandLo && day !== bandHi) || undefined}
                    className={cn(
                      'p-0 text-center',
                      inBand && 'bg-primary/10',
                      inBand && day === bandLo && 'rounded-l-md',
                      inBand && day === bandHi && 'rounded-r-md',
                    )}
                  >
                    <button
                      type="button"
                      data-date={day}
                      data-autofocus={autoFocus && day === roving ? '' : undefined}
                      tabIndex={day === roving ? 0 : -1}
                      aria-label={dayFormat.format(new Date(toUtc(day)))}
                      aria-current={day === today ? 'date' : undefined}
                      aria-disabled={blocked || undefined}
                      onMouseEnter={() => setHover(blocked ? '' : day)}
                      onFocus={() => setFocusDay(day)}
                      onClick={() => {
                        if (blocked) return
                        setFocusDay(day)
                        onPick(day)
                      }}
                      className={cn(
                        'size-9 w-full rounded-md text-sm tabular-nums outline-none transition-colors focus-visible:ring-2 focus-visible:ring-primary/50',
                        edge ? 'bg-primary font-medium text-primary-foreground' : blocked ? 'cursor-not-allowed text-muted-foreground/40' : 'hover:bg-accent',
                        !edge && !blocked && day === today && 'font-semibold ring-1 ring-inset ring-primary/40',
                      )}
                    >
                      {Number(day.slice(8))}
                    </button>
                  </td>
                )
              })}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  )
}
