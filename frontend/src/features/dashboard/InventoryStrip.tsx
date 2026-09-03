import { useQuery } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { Boxes, KeyRound, Server, Users } from 'lucide-react'
import type { LucideIcon } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { Card, CardContent } from '@/components/ui/card'
import type { InventoryResp } from '@/features/analytics/types'
import { apiFetch } from '@/lib/api'
import { formatCount } from '@/lib/money'
import { qk } from '@/lib/query-keys'
import { cn } from '@/lib/utils'

function Item({
  icon: Icon,
  label,
  value,
  sub,
  to,
  tone,
}: {
  icon: LucideIcon
  label: string
  value: string
  sub: string
  to: string
  tone?: 'warn' | 'bad'
}) {
  return (
    <Link
      to={to}
      className="flex min-w-0 flex-1 items-center gap-3 rounded-md px-2 py-1 transition-colors hover:bg-muted/60"
    >
      <Icon
        className={cn(
          'h-4 w-4 shrink-0',
          tone === 'bad' ? 'text-destructive' : tone === 'warn' ? 'text-warning' : 'text-muted-foreground',
        )}
      />
      <div className="flex min-w-0 flex-col leading-tight">
        <span className="text-xs text-muted-foreground">{label}</span>
        <span className="flex flex-wrap items-baseline gap-x-1.5">
          <span className="font-semibold tabular-nums">{value}</span>
          <span
            className={cn(
              'truncate text-xs',
              tone === 'bad' ? 'text-destructive' : tone === 'warn' ? 'text-warning' : 'text-muted-foreground',
            )}
          >
            {sub}
          </span>
        </span>
      </div>
    </Link>
  )
}

/// 站点规模条：用户 / 密钥 / 渠道 / 模型四个实体的存量与健康（Sub2API DashboardStats
/// 的实体计数区 + 老 ok-api Overview 的 channels total/active/healthy）。
///
/// 纯 PG 计数，CH 未启用也照常显示——最小部署的落地页此前除了实时条什么都没有。
/// 每项可点进对应管理页；渠道项在"启用但零可用 key"时转黄、有自动停用时转红：
/// 渠道级开关绿着、key 全在冷却的渠道，在这里就该被看见。
export function InventoryStrip() {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const q = useQuery({
    queryKey: qk.statsInventory,
    queryFn: () => apiFetch<InventoryResp>('/admin/stats/inventory'),
    staleTime: 60_000,
    retry: false,
  })
  if (q.isError || !q.data) return null
  const d = q.data
  const n = (v: number) => formatCount(v, locale)
  const channelTone = d.channels.auto_disabled > 0 ? 'bad' : d.channels.no_key > 0 ? 'warn' : undefined
  const channelSub =
    d.channels.auto_disabled > 0
      ? t('admin:invChannelsAutoDisabled', { n: n(d.channels.auto_disabled) })
      : d.channels.no_key > 0
        ? t('admin:invChannelsNoKey', { n: n(d.channels.no_key) })
        : t('admin:invChannelsHealthy', { n: n(d.channels.healthy), total: n(d.channels.total) })
  const unpriced = d.models.total - d.models.priced

  return (
    <Card>
      <CardContent className="flex flex-wrap items-center gap-x-2 gap-y-1 py-2">
        <span className="px-2 text-xs font-medium text-muted-foreground">{t('admin:invTitle')}</span>
        <Item
          icon={Users}
          label={t('admin:invUsers')}
          value={n(d.users.total)}
          sub={
            d.users.new_today > 0
              ? t('admin:invUsersNewToday', { n: n(d.users.new_today) })
              : t('admin:invUsersNew7d', { n: n(d.users.new_7d) })
          }
          to="/admin/users"
        />
        <Item
          icon={KeyRound}
          label={t('admin:invKeys')}
          value={n(d.api_keys.active)}
          sub={t('admin:invKeysUsed7d', { n: n(d.api_keys.used_7d) })}
          to="/admin/keys"
        />
        <Item
          icon={Server}
          label={t('admin:invChannels')}
          value={n(d.channels.total)}
          sub={channelSub}
          tone={channelTone}
          to="/admin/channels"
        />
        <Item
          icon={Boxes}
          label={t('admin:invModels')}
          value={n(d.models.total)}
          sub={
            unpriced > 0
              ? t('admin:invModelsUnpriced', { n: n(unpriced) })
              : t('admin:invModelsServed', { n: n(d.models.served) })
          }
          tone={unpriced > 0 ? 'warn' : undefined}
          to="/admin/pricing"
        />
      </CardContent>
    </Card>
  )
}
