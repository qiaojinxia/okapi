import { useQuery } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { Badge } from '@/components/ui/badge'
import type { PoolDetail } from '@/features/pools/types'
import { apiFetch } from '@/lib/api'
import { qk } from '@/lib/query-keys'

const MAX_MODELS = 12

/// 一个池"能打到什么"的只读摘要：成员渠道数 / 可用 key 数 / 能服务的模型 / 降级池。
///
/// 放在分组抽屉里（选池的下方）与池抽屉里：三跳关系（分组 → 池 → 渠道 → 模型）此前
/// 要在三个页面之间来回拼，选了一个池却不知道它给不给得出用户要的模型。
export function PoolReach({ poolCode }: { poolCode: string }) {
  const { t } = useTranslation()
  const q = useQuery({
    queryKey: qk.poolDetail(poolCode),
    queryFn: () => apiFetch<PoolDetail>(`/admin/pools/${encodeURIComponent(poolCode)}`),
    enabled: poolCode !== '',
    staleTime: 30_000,
  })
  if (poolCode === '' || q.isError) return null
  const d = q.data
  if (!d) {
    return <p className="text-xs text-muted-foreground">{t('common:loading')}</p>
  }
  const members = d.members ?? []
  const models = d.models ?? []
  const groups = d.groups ?? []
  const enabled = members.filter((m) => m.status === 1 && m.enabled !== false)
  const reachable = enabled.filter((m) => m.active_keys > 0)
  const shown = models.slice(0, MAX_MODELS)
  const more = models.length - shown.length
  const fallbacks =
    d.policy?.fallback_pools && d.policy.fallback_pools.length > 0
      ? d.policy.fallback_pools
      : d.fallback_pool_code
        ? [d.fallback_pool_code]
        : []

  return (
    <div className="flex flex-col gap-2 rounded-md border border-border bg-muted/30 p-3 text-xs">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
        <span className="text-muted-foreground">
          {t('admin:poolReachChannels', { n: enabled.length, ok: reachable.length })}
        </span>
        {fallbacks.length > 0 && (
          <span className="text-muted-foreground">
            {t('admin:poolReachFallback', { pool: fallbacks.join(' → ') })}
          </span>
        )}
        {groups.length > 0 && (
          <span className="text-muted-foreground">
            {t('admin:poolReachGroups', { groups: groups.join(', ') })}
          </span>
        )}
      </div>
      {enabled.length === 0 ? (
        <Badge variant="destructive" className="self-start">
          {t('admin:poolNoChannel')}
        </Badge>
      ) : (
        <div className="flex flex-wrap gap-1">
          {shown.map((m) => (
            <Badge key={m} variant="muted" className="font-mono">
              {m}
            </Badge>
          ))}
          {more > 0 && <Badge variant="muted">+{more}</Badge>}
        </div>
      )}
    </div>
  )
}
