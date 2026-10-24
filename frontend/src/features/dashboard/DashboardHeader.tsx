import { Link } from '@tanstack/react-router'
import { ArrowUpRight, CircleHelp, RefreshCw } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { DateRangePicker } from '@/components/ui/date-range'
import type { DateRange } from '@/components/ui/date-range'
import { Segmented } from '@/components/ui/segmented'
import { Tooltip } from '@/components/ui/tooltip'
import { HealthSummary } from './AttentionCard'
import { LiveStatus } from './LiveStatus'
import { dashboardSearch } from './period'

/// 副行里分隔各段状态的小圆点。
function Dot() {
  return <span aria-hidden className="size-[3px] shrink-0 rounded-full bg-muted-foreground/40" />
}

/// 页头：标题、一行状态（统计日历 · 实时流量 · 服务健康）、周期选择与操作。
/// 日历日期来自服务端（不是浏览器的日期），实时和健康是当前状态，不随历史日期切换。
export function DashboardHeader({ title, days, range, calendar, today, refreshing, onRefresh, onPeriod, onRange }: {
  title: string
  days: number
  range: DateRange | null
  calendar?: { start: string; end: string; timezone: string }
  today?: string
  refreshing: boolean
  onRefresh: () => void
  onPeriod: (days: number) => void
  onRange: (range: DateRange) => void
}) {
  const { t } = useTranslation()
  return <header data-slot="dashboard-header" className="flex flex-wrap items-end gap-x-4 gap-y-3">
    <div className="flex min-w-[16rem] flex-1 flex-col gap-1.5">
      <h1 className="text-xl leading-7 font-bold tracking-tight">{title}</h1>
      <div className="flex flex-wrap items-center gap-x-2.5 gap-y-1 text-[13px] leading-5 text-muted-foreground">
        {calendar && <>
          <span data-slot="dashboard-calendar" className="tabular-nums">{t('admin:dashboardCalendar', { start: calendar.start, end: calendar.end, timezone: calendar.timezone })}</span>
          <Dot />
        </>}
        <LiveStatus />
        <Dot />
        <HealthSummary />
      </div>
    </div>
    <div className="flex min-w-0 max-w-full flex-wrap items-center gap-2">
      <Segmented size="sm" ariaLabel={t('charts:period')} value={range ? 'custom' : String(days)}
        onChange={(value) => onPeriod(Number(value))}
        options={[1, 7, 30].map((value) => ({ value: String(value), label: value === 1 ? t('admin:kpiToday') : t('admin:lastDays', { days: value }) }))} />
      <Tooltip content={t('admin:dashboardPeriodHint')}>
        <button type="button" aria-label={t('admin:dashboardScope')} className="flex h-8 w-8 items-center justify-center rounded text-muted-foreground outline-none hover:bg-muted focus-visible:ring-2 focus-visible:ring-primary/40"><CircleHelp className="h-4 w-4" /></button>
      </Tooltip>
      {today && <DateRangePicker today={today} value={range} closeOnApply onApply={onRange} className="min-w-0 max-w-full px-3 py-1 text-[13px] [&>summary]:min-h-6" />}
      <Button variant="outline" size="icon" loading={refreshing} aria-label={t('common:refresh')} title={t('common:refresh')} onClick={onRefresh} className="size-[34px] rounded-lg">
        {!refreshing && <RefreshCw className="h-[15px] w-[15px]" />}
      </Button>
      <Link to="/admin/stats" search={dashboardSearch(days, range)} className="inline-flex min-h-10 items-center gap-1.5 rounded-lg bg-primary px-3.5 text-[13px] font-semibold text-primary-foreground outline-none transition-colors hover:bg-primary/90 focus-visible:ring-2 focus-visible:ring-primary/40 md:min-h-[34px]">
        {t('admin:dashboardAnalyze')}<ArrowUpRight className="h-[13px] w-[13px]" />
      </Link>
    </div>
  </header>
}
