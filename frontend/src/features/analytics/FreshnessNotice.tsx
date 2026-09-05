import { useTranslation } from 'react-i18next'
import type { Freshness } from './types'
import { cn } from '@/lib/utils'
export function FreshnessNotice({ value }: { value?: Freshness }) {
  const { t, i18n } = useTranslation()
  if (!value) return null
  const time = (v: string | null) => v ? new Date(v).toLocaleString(i18n.language) : t('analysis:notCollected')
  return <div role="status" className={cn('flex flex-wrap gap-x-4 gap-y-1 rounded-lg px-3 py-2 text-xs', value.stale ? 'bg-warning/10 text-warning' : 'bg-muted/40 text-muted-foreground')}>
    <span>{t('analysis:lastEvent', { time: time(value.last_event_at) })}</span><span>{t('analysis:lastIngested', { time: time(value.last_ingested_at) })}</span>
    {(value.pending_events > 0 || value.failed_events > 0 || value.stale) && <span>{t('analysis:backlog', { n: value.pending_events, failed: value.failed_events, age: value.queue_age_seconds ?? value.event_gap_seconds ?? 0 })}</span>}
    <span>{t(value.stale ? 'analysis:delayed' : 'analysis:siteFreshness')}</span>
  </div>
}
