import { useQuery } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { ChevronDown } from 'lucide-react'
import { Badge } from '@/components/ui/badge'
import { Card } from '@/components/ui/card'
import { InlineStat } from '@/components/ui/stat'
import { DEFAULT_PAGE_SIZE } from '@/hooks/use-pagination'
import { apiFetch } from '@/lib/api'
import { formatBp, formatCount, formatMoneyAggregate } from '@/lib/money'
import { qk } from '@/lib/query-keys'
import { ExtraMetrics } from '@/features/logs/LogSummary'
import type { StatResp } from './types'
import { toParams } from './search'
import type { Draft } from './search'

/// 统计条：消耗 / 请求 / 错误率 / 缓存命中 / RPM / TPM 一行速览
/// （new-api 日志页 Stat 语义；RPM/TPM 数据源由后端按"是否带过滤"切换）。
export function StatBar({ applied }: { applied: Draft }) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const [expanded, setExpanded] = useState(false)
  // 统计不分页：使用固定参数，不随明细表换页宽而重新请求。
  const params = toParams(applied, 0, DEFAULT_PAGE_SIZE)
  const q = useQuery({
    queryKey: qk.adminLogStat(params),
    queryFn: () => apiFetch<StatResp>(`/admin/logs/stat?${params}`),
    refetchInterval: 15_000,
    retry: false,
  })

  if (q.isError) {
    // CH 未启用时统计条静默收起，不挡明细排障（明细也会 501，但让表格报即可）
    return null
  }
  const s = q.data
  const cell = (label: string, value: string, tone?: 'warn' | 'bad') => (
    <InlineStat label={label} value={value} tone={tone ?? 'default'} className="px-3 py-2" />
  )
  const errBp = s?.error_rate_bp ?? 0
  return (
    <Card role="region" aria-label={t('logs:summary')} data-slot="admin-log-summary" className="shrink-0 overflow-hidden rounded-xl">
      <div className="flex min-w-0 flex-wrap items-center">
        <div className="grid min-w-0 flex-1 basis-[40rem] grid-cols-4 md:grid-cols-8 [&>div:not(:first-child)]:border-l [&>div]:border-border/60">
          {cell(t('admin:logsStatSpend'), s ? formatMoneyAggregate(s.amount_micro, locale) : '—')}
          {cell(t('common:requests'), s ? formatCount(s.requests, locale) : '—')}
          {cell(
            t('admin:statErrorRate'),
            s ? formatBp(errBp, locale) : '—',
            errBp >= 500 ? 'bad' : errBp >= 100 ? 'warn' : undefined,
          )}
          {cell(t('admin:kpiTokens'), s ? formatCount(s.tokens, locale) : '—')}
          {cell(t('admin:logsStatCacheHit'), s?.cache_hit_bp != null ? formatBp(s.cache_hit_bp, locale) : '—')}
          {cell(t('admin:logsStatUsers'), s ? formatCount(s.users, locale) : '—')}
          {cell('RPM', s ? formatCount(s.rpm, locale) : '—')}
          {cell('TPM', s ? formatCount(s.tpm, locale) : '—')}
        </div>
        <div className="flex w-full shrink-0 items-center justify-end gap-3 border-t border-border/60 px-3 py-1.5 md:w-auto md:flex-col md:items-end md:gap-1 md:border-t-0 md:py-2">
          {s && (
            <Badge variant="muted" title={t('admin:logsRateSourceHint')}>
              {s.rate_source === 'redis' ? t('admin:logsRateLive') : t('admin:logsRateWindow')}
            </Badge>
          )}
          <button type="button" aria-expanded={expanded} onClick={() => setExpanded(!expanded)} className="flex items-center gap-1 rounded text-xs text-primary outline-none focus-visible:ring-2 focus-visible:ring-primary/40">
            {t('logs:moreMetrics')}<ChevronDown aria-hidden className={`h-3.5 w-3.5 transition-transform ${expanded ? 'rotate-180' : ''}`} />
          </button>
        </div>
      </div>
      <ExtraMetrics data={s} expanded={expanded} />
    </Card>
  )
}
