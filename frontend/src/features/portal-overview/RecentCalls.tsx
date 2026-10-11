import { useQuery } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import dayjs from 'dayjs'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Card } from '@/components/ui/card'
import { Segmented } from '@/components/ui/segmented'
import { Skeleton } from '@/components/ui/skeleton'
import type { PortalLogSearch } from '@/features/logs/search'
import type { LogsResp } from '@/features/logs/types'
import { apiFetch } from '@/lib/api'
import { qk } from '@/lib/query-keys'
import { cn } from '@/lib/utils'
import { formatUsd } from './format'
import type { Scope } from './types'

/// 最近调用 / 最近失败：总览右下角填上「刚才发生了什么」，点进去就是明细。
export function RecentCalls({ scope, logSearch }: { scope: Scope; logSearch: PortalLogSearch }) {
  const { t, i18n } = useTranslation()
  const [errors, setErrors] = useState(false)
  const params = `scope=${scope}&limit=6${errors ? '&errors_only=true' : ''}`
  const q = useQuery({
    queryKey: [...qk.logs(params), 'overview'],
    queryFn: () => apiFetch<LogsResp>(`/api/me/logs?${params}`),
    retry: false,
    refetchInterval: 30_000,
  })
  const rows = q.data?.data ?? []
  return <Card className="flex h-full min-w-0 flex-col rounded-xl px-4 py-3">
    <section aria-label={t(errors ? 'portal:recentErrors' : 'portal:recentCalls')} className="flex flex-1 flex-col gap-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h2 className="text-sm font-semibold">{t(errors ? 'portal:recentErrors' : 'portal:recentCalls')}</h2>
        <Segmented size="sm" ariaLabel={t('portal:recentCalls')} value={errors ? 1 : 0} onChange={(v) => setErrors(v === 1)}
          options={[{ value: 0, label: t('portal:recentAll') }, { value: 1, label: t('portal:recentFailed') }]} />
      </div>
      {q.isPending ? <div className="space-y-2">{[0, 1, 2, 3].map((i) => <Skeleton key={i} className="h-8 w-full" />)}</div>
        : q.isError ? <p className="text-xs text-muted-foreground">{t('charts:statisticsUnavailable')}</p>
          : rows.length === 0 ? <p className="py-2 text-xs text-muted-foreground">{t(errors ? 'portal:noRecentErrors' : 'portal:noRecentCalls')}</p>
            : <ul className="divide-y divide-border">
              {rows.map((row) => {
                const failed = row.is_error || row.status === 40 || row.log_type === 5
                return <li key={row.id} className="flex min-w-0 items-center gap-2 py-2 text-sm">
                  <span aria-hidden className={cn('h-1.5 w-1.5 shrink-0 rounded-full', failed ? 'bg-destructive' : 'bg-success')} />
                  <span className="min-w-0 flex-1">
                    <span className="block truncate" title={row.model}>{row.model}</span>
                    <span className="block truncate text-xs text-muted-foreground">
                      {dayjs(row.created_at).format('MM-DD HH:mm')}{failed && row.error_code ? ` · ${row.error_code}` : ''}
                    </span>
                  </span>
                  <span className="shrink-0 tabular-nums text-muted-foreground">{formatUsd(row.amount_micro, i18n.language)}</span>
                </li>
              })}
            </ul>}
      <Link to="/portal/logs" search={{ ...logSearch, errors_only: errors ? true : undefined }} className="mt-auto self-start rounded text-xs font-medium text-primary outline-none hover:underline focus-visible:ring-2 focus-visible:ring-primary/40">
        {t('portal:allCalls')}
      </Link>
    </section>
  </Card>
}
