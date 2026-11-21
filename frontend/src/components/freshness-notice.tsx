import { useTranslation } from 'react-i18next'
import type { Freshness } from '@/lib/cube'
import { cn } from '@/lib/utils'
export function FreshnessNotice({ value }: { value?: Freshness }) {
  const { t, i18n } = useTranslation()
  if (!value) return null
  const time = (v: string | null) => v ? new Date(v).toLocaleString(i18n.language) : t('analysis:notCollected')
  return <div role="status" className={cn('flex flex-wrap items-center gap-x-4 gap-y-1 rounded-xl px-3.5 py-1.5 text-[11.5px] tabular-nums ring-1 ring-inset', value.stale ? 'bg-warning/10 text-warning ring-warning/25' : 'bg-muted/35 text-muted-foreground ring-border/60')}>
    <span className="inline-flex items-center gap-1.5">
      <span aria-hidden className={cn('h-1.5 w-1.5 shrink-0 rounded-full', value.stale ? 'bg-warning' : 'bg-success/75')} />
      {t('analysis:lastEvent', { time: time(value.last_event_at) })}
    </span>
    <span>{t('analysis:lastIngested', { time: time(value.last_ingested_at) })}</span>
    {(value.pending_events > 0 || value.failed_events > 0 || value.stale) && <span>{t('analysis:backlog', { n: value.pending_events, failed: value.failed_events, age: value.queue_age_seconds ?? value.event_gap_seconds ?? 0 })}</span>}
    <span className="ml-auto opacity-80">{t(value.stale ? 'analysis:delayed' : 'analysis:siteFreshness')}</span>
  </div>
}
