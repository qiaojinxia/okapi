import { useQuery } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import dayjs from 'dayjs'
import { useTranslation } from 'react-i18next'
import type { EntityUsage, EntityUsageResp } from '@/features/analytics/types'
import { usePermission } from '@/hooks/use-auth'
import { apiFetch } from '@/lib/api'
import { formatMoney } from '@/lib/money'
import { qk } from '@/lib/query-keys'

const DAYS = 7

/// 列表页当页实体的行内用量（Sub2API 用户 / key 列表每行直接显示 today / total 消费）。
///
/// 一次请求带上当页全部 id，单维 MV 前缀点查；无 billing.read 不发请求（列也不渲染），
/// CH 未启用时 501 → 静默给"—"。ids 去重排序后作 query key，翻页即换。
export function useEntityUsage(kind: 'user' | 'api_key', ids: number[]) {
  const can = usePermission()
  const allowed = can('billing.read')
  const key = [...new Set(ids)].sort((a, b) => a - b).join(',')
  const q = useQuery({
    queryKey: qk.entityUsage(kind, key, DAYS),
    queryFn: () =>
      apiFetch<EntityUsageResp>(`/admin/stats/entity-usage?kind=${kind}&ids=${key}&days=${DAYS}`),
    enabled: allowed && key !== '',
    retry: false,
    staleTime: 60_000,
  })
  return { enabled: allowed, data: q.data?.data, unavailable: q.isError }
}

/// 一格两行："今日 $x"主行，"7 天 $y · 最近 MM-DD"副行；点击进用量分析（已按该实体过滤）。
export function UsageCell({
  usage,
  unavailable,
  link,
}: {
  usage: EntityUsage | undefined
  unavailable: boolean
  link: { user_id?: number; api_key_id?: number }
}) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  if (unavailable) return <span className="text-xs text-muted-foreground">—</span>
  if (usage === undefined) {
    return <span className="text-xs text-muted-foreground">{t('admin:usageNone', { days: DAYS })}</span>
  }
  const last = usage.last_day ? dayjs(usage.last_day) : null
  return (
    <Link
      to="/admin/stats"
      search={link}
      className="flex flex-col leading-tight hover:underline"
      title={t('admin:usageOpenAnalytics')}
    >
      <span className="tabular-nums">
        {t('admin:usageToday', { v: formatMoney(usage.today_micro, locale) })}
      </span>
      <span className="text-xs text-muted-foreground tabular-nums">
        {t('admin:usageWindow', { days: DAYS, v: formatMoney(usage.window_micro, locale) })}
        {last && !last.isSame(dayjs(), 'day') ? ` · ${last.format('MM-DD')}` : ''}
      </span>
    </Link>
  )
}
