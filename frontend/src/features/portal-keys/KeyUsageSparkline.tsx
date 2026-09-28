import { useTranslation } from 'react-i18next'
import { Tooltip } from '@/components/ui/tooltip'
import { formatCount } from '@/lib/money'

export interface KeyTokenTrend {
  days: string[]
  tokens: number[]
  timezone: string
}

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
  const valid = trend?.timezone === 'UTC' && Array.isArray(trend.days) && Array.isArray(trend.tokens)
    && trend.days.length === 7 && trend.tokens.length === 7
    && trend.days.every((day) => /^\d{4}-\d{2}-\d{2}$/.test(day))
    && trend.tokens.every((value) => Number.isSafeInteger(value) && value >= 0)
  const total = valid ? trend.tokens.reduce((sum, value) => sum + value, 0) : 0
  const points = valid ? tokenTrendPoints(trend.tokens) : ''
  const hint = valid
    ? `${t('portal:keyTrendHint', { start: trend.days[0], end: trend.days[6], n: formatCount(total, i18n.language) })} ${trend.days.map((day, i) => `${day.slice(5)}: ${formatCount(trend.tokens[i], i18n.language)}`).join(' · ')}`
    : t(trend === undefined ? 'portal:keyTrendUpgradeHint' : 'portal:keyTrendUnavailable')

  return <Tooltip content={hint} className="w-full min-w-0 max-w-40">
    <button
      type="button"
      data-slot="key-usage-sparkline"
      data-state={!valid ? 'unavailable' : total === 0 ? 'empty' : 'ready'}
      aria-label={t('portal:keyTrendOpen', { name, id })}
      aria-haspopup="dialog"
      onClick={onClick}
      className="relative flex h-8 w-full min-w-0 max-w-40 items-center gap-1 rounded-md text-primary outline-none transition-colors hover:bg-primary/8 focus-visible:ring-2 focus-visible:ring-primary/40"
    >
      {valid ? <>
        <span className="w-11 shrink-0 truncate text-left text-xs font-semibold tabular-nums text-foreground" data-slot="key-trend-total">{formatCount(total, i18n.language)}</span>
        <span className="relative h-7 min-w-0 max-w-26 flex-1">
        <svg aria-hidden viewBox="0 0 104 28" preserveAspectRatio="none" className="h-7 w-full overflow-visible">
        <path d="M3 25H101" fill="none" stroke="currentColor" strokeOpacity="0.12" />
        {total > 0 && <polygon points={`3,25 ${points} 101,25`} fill="currentColor" fillOpacity="0.08" />}
        <polyline points={points} fill="none" stroke="currentColor" strokeWidth={1.8} strokeLinecap="round" strokeLinejoin="round" strokeOpacity={total > 0 ? 1 : 0.3} strokeDasharray={total > 0 ? undefined : '3 3'} />
        {total > 0 && <circle cx={101} cy={25 - trend.tokens[6] / Math.max(1, ...trend.tokens) * 22} r={2.5} fill="currentColor" />}
        </svg>
        {total === 0 && <span className="absolute inset-x-0 top-0 text-center text-[10px] text-muted-foreground">{t('portal:keyTrendEmpty')}</span>}
        </span>
      </> : <span className="truncate text-xs text-muted-foreground">
        {t(trend === undefined ? 'portal:keyTrendUpgrade' : 'portal:keyTrendMissing')}
      </span>}
    </button>
  </Tooltip>
}
