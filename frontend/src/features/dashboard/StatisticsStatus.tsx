import type { UseQueryResult } from '@tanstack/react-query'
import { AlertTriangle, Clock3, RefreshCw } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { Tooltip } from '@/components/ui/tooltip'
import type { TrendResp } from '@/features/analytics/types'
import { cn } from '@/lib/utils'

// 查询刚完成不等于入库已追平；使用服务端的队列状态单独提醒数据缺口。
export function StatisticsStatus({ query }: { query: UseQueryResult<TrendResp> }) {
  const { t, i18n } = useTranslation()
  const freshness = query.isError ? undefined : query.data?.window?.freshness
  const delayed = !!freshness && (freshness.stale || freshness.pending_events > 0 || freshness.failed_events > 0)
  const time = query.dataUpdatedAt > 0 ? new Date(query.dataUpdatedAt).toLocaleTimeString(i18n.language, { hour: '2-digit', minute: '2-digit' }) : ''
  const Icon = query.isError || delayed ? AlertTriangle : query.isFetching ? RefreshCw : Clock3
  const label = query.isError ? t('admin:dashboardStatsFailed') : delayed ? t('admin:dashboardStatsDelayed')
    : query.isPending ? t('common:loading') : t('admin:dashboardStatsUpdated', { time })
  const eventTime = (value: string | null) => value ? new Date(value).toLocaleString(i18n.language) : t('analysis:notCollected')
  const description = [
    t('admin:dashboardStatsRefreshHint'),
    !query.isError && time ? t('charts:freshness', { time }) : '',
    freshness ? t('analysis:lastEvent', { time: eventTime(freshness.last_event_at) }) : '',
    freshness ? t('analysis:lastIngested', { time: eventTime(freshness.last_ingested_at) }) : '',
    delayed ? t('analysis:backlog', { n: freshness.pending_events, failed: freshness.failed_events, age: freshness.queue_age_seconds ?? freshness.event_gap_seconds ?? 0 }) : '',
  ].filter(Boolean).join(' · ')
  return <Tooltip content={description}>
    <button type="button" aria-label={label} className={cn('inline-flex min-h-11 items-center gap-1 rounded text-xs outline-none focus-visible:ring-2 focus-visible:ring-primary/40 sm:min-h-7', query.isError || delayed ? 'text-warning' : 'text-muted-foreground')}>
      <Icon aria-hidden className={cn('h-3 w-3 shrink-0', query.isFetching && !delayed && !query.isError && 'animate-spin motion-reduce:animate-none')} />
      {label}
    </button>
  </Tooltip>
}
