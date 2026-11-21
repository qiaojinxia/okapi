import { useTranslation } from 'react-i18next'
import { Tooltip } from '@/components/ui/tooltip'
import { formatCount } from '@/lib/money'
import { cn } from '@/lib/utils'
import type { KeyTokenTrend } from './api'

export function tokenTrendPoints(tokens: number[]): string {
  const max = Math.max(1, ...tokens)
  return tokens.map((value, i) => `${3 + i * 98 / 6},${25 - value / max * 22}`).join(' ')
}

export function KeyUsageSparkline({ trend, name, id, onClick }: {
  trend?: KeyTokenTrend | null
  name: string
  id: number
  onClick: () => void
}) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const valid = trend?.timezone === 'UTC' && Array.isArray(trend.days) && Array.isArray(trend.tokens)
    && trend.days.length === 7 && trend.tokens.length === 7
    && trend.days.every((day) => /^\d{4}-\d{2}-\d{2}$/.test(day))
    && trend.tokens.every((value) => Number.isSafeInteger(value) && value >= 0)
  const total = valid ? trend.tokens.reduce((sum, value) => sum + value, 0) : 0
  const maxDay = valid ? Math.max(...trend.tokens) : 0
  const points = valid ? tokenTrendPoints(trend.tokens) : ''

  const tooltipContent = valid ? (
    <div className="w-60 space-y-2.5 text-left">
      <div className="space-y-0.5 border-b border-border/60 pb-2">
        <div className="flex items-center justify-between gap-2 text-[11px] text-muted-foreground tabular-nums">
          <span>{trend.days[0]} — {trend.days[6]}</span>
          <span className="rounded bg-muted/80 px-1 py-px font-mono text-[10px] font-medium text-muted-foreground">
            {trend.timezone}
          </span>
        </div>
        <div className="text-[13px] font-semibold text-foreground tabular-nums">
          {t('portal:keyTrendTotal', { n: formatCount(total, locale) })}
        </div>
      </div>

      <div className="space-y-1.5">
        {trend.days.map((day, i) => {
          const val = trend.tokens[i]
          const isPeak = maxDay > 0 && val === maxDay
          const widthPct = maxDay > 0 ? Math.max(val > 0 ? 4 : 0, Math.round((val / maxDay) * 100)) : 0
          return (
            <div key={day} className="flex items-center gap-2.5 text-[11.5px] leading-4 tabular-nums">
              <span className={cn('w-10 shrink-0 font-mono text-[11px]', isPeak ? 'font-medium text-foreground' : 'text-muted-foreground')}>
                {day.slice(5)}
                <span className="sr-only">: </span>
              </span>
              <span aria-hidden className="h-1.5 min-w-0 flex-1 overflow-hidden rounded-full bg-muted/85">
                {widthPct > 0 && (
                  <span
                    className={cn('block h-full rounded-full', isPeak ? 'bg-primary' : 'bg-primary/45')}
                    style={{ width: `${widthPct}%` }}
                  />
                )}
              </span>
              <span
                className={cn(
                  'min-w-12 shrink-0 text-right whitespace-nowrap',
                  val === 0
                    ? 'text-muted-foreground/55'
                    : isPeak
                      ? 'font-semibold text-foreground'
                      : 'font-medium text-foreground/85',
                )}
              >
                {formatCount(val, locale)}
              </span>
            </div>
          )
        })}
      </div>

      <div className="flex items-center justify-between gap-2 border-t border-border/60 pt-2 text-[10.5px] leading-snug text-muted-foreground/85">
        <span>{t('portal:keyTrendBasis')}</span>
      </div>
    </div>
  ) : t(trend === undefined ? 'portal:keyTrendUpgradeHint' : 'portal:keyTrendUnavailable')

  return (
    <Tooltip content={tooltipContent} className="w-full min-w-0 max-w-40">
      <button
        type="button"
        data-slot="key-usage-sparkline"
        data-state={!valid ? 'unavailable' : total === 0 ? 'empty' : 'ready'}
        aria-label={t('portal:keyTrendOpen', { name, id })}
        aria-haspopup="dialog"
        onClick={onClick}
        className="relative flex h-8 w-full min-w-0 max-w-40 cursor-pointer items-center gap-1 rounded-md text-primary outline-none transition-colors hover:bg-primary/8 focus-visible:ring-2 focus-visible:ring-primary/40"
      >
        {valid ? (
          <>
            <span className="w-11 shrink-0 truncate text-left text-xs font-semibold tabular-nums text-foreground" data-slot="key-trend-total">{formatCount(total, locale)}</span>
            <span className="relative h-7 min-w-0 max-w-26 flex-1">
              <svg aria-hidden viewBox="0 0 104 28" preserveAspectRatio="none" className="h-7 w-full overflow-visible">
                <path d="M3 25H101" fill="none" stroke="currentColor" strokeOpacity="0.12" />
                {total > 0 && <polygon points={`3,25 ${points} 101,25`} fill="currentColor" fillOpacity="0.08" />}
                <polyline points={points} fill="none" stroke="currentColor" strokeWidth={1.8} strokeLinecap="round" strokeLinejoin="round" strokeOpacity={total > 0 ? 1 : 0.3} strokeDasharray={total > 0 ? undefined : '3 3'} />
                {total > 0 && <circle cx={101} cy={25 - trend.tokens[6] / Math.max(1, ...trend.tokens) * 22} r={2.5} fill="currentColor" />}
              </svg>
              {total === 0 && <span className="absolute inset-x-0 top-0 text-center text-[10px] text-muted-foreground">{t('portal:keyTrendEmpty')}</span>}
            </span>
          </>
        ) : (
          <span className="truncate text-xs text-muted-foreground">
            {t(trend === undefined ? 'portal:keyTrendUpgrade' : 'portal:keyTrendMissing')}
          </span>
        )}
      </button>
    </Tooltip>
  )
}
