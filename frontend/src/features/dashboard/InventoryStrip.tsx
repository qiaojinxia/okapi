import { Link } from '@tanstack/react-router'
import { Boxes, ChevronRight, KeyRound, Server, TriangleAlert, Users } from 'lucide-react'
import type { LucideIcon } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { ErrorState, LoadingState } from '@/components/ui/state'
import { describeError } from '@/lib/i18n'
import { formatCount } from '@/lib/money'
import { useDashboardInventory } from './data'
import { cn } from '@/lib/utils'
import { Tooltip } from '@/components/ui/tooltip'

function Item({
  icon: Icon,
  label,
  value,
  sub,
  to,
  tone,
  compact = false,
  summary,
}: {
  icon: LucideIcon
  label: string
  value: string
  sub: string
  to: string
  tone?: 'warn' | 'bad'
  compact?: boolean
  summary?: string
}) {
  if (compact) return <Tooltip content={sub} className="min-w-0"><Link to={to} aria-label={`${label} ${value} · ${sub}`}
    className="grid min-h-11 w-full min-w-0 grid-cols-[minmax(0,1fr)_auto] items-center gap-x-1 rounded-md px-2 py-1 text-xs outline-none hover:bg-muted/60 focus-visible:ring-2 focus-visible:ring-primary/40 xl:flex xl:min-h-8 xl:flex-wrap xl:gap-x-1.5 xl:py-0">
    <span className="flex min-w-0 flex-wrap items-center gap-x-1.5">
      <Icon aria-hidden className={cn('h-3.5 w-3.5 shrink-0', tone === 'bad' ? 'text-destructive' : tone === 'warn' ? 'text-warning' : 'text-muted-foreground')} />
      <span className="text-muted-foreground">{label}</span>
      <span className="whitespace-nowrap font-semibold tabular-nums">{value}</span>
    </span>
    <span className={cn('col-start-1 inline-flex min-w-0 items-center gap-1 text-[11px] xl:ml-auto', tone === 'bad' ? 'text-destructive' : tone === 'warn' ? 'text-warning' : 'text-muted-foreground')}>
      {tone && <TriangleAlert aria-hidden className="h-3 w-3 shrink-0" />}
      <span className="break-words">{summary ?? sub}</span>
    </span>
    <ChevronRight aria-hidden className="col-start-2 row-span-2 row-start-1 h-3 w-3 shrink-0 text-muted-foreground" />
  </Link></Tooltip>
  return (
    <Link
      to={to}
      className="group relative flex min-w-0 items-start gap-2 rounded-lg px-2 py-1 outline-none transition-colors hover:bg-muted/60 focus-visible:ring-2 focus-visible:ring-primary/40"
    >
      <Icon
        className={cn(
          'mt-0.5 h-4 w-4 shrink-0',
          tone === 'bad' ? 'text-destructive' : tone === 'warn' ? 'text-warning' : 'text-muted-foreground',
        )}
      />
      <div className="flex min-w-0 flex-1 flex-col leading-tight">
        <span className="flex flex-wrap items-baseline gap-x-2">
          <span className="text-xs text-muted-foreground">{label}</span>
          <span className="text-base font-semibold tabular-nums">{value}</span>
        </span>
        <span
            className={cn(
              'w-full break-words text-xs leading-5',
              tone === 'bad' ? 'text-destructive' : tone === 'warn' ? 'text-warning' : 'text-muted-foreground',
            )}
          >
            {sub}
        </span>
      </div>
      <ChevronRight className="mt-0.5 h-3.5 w-3.5 shrink-0 text-muted-foreground group-hover:text-primary" />
    </Link>
  )
}

/// 站点规模条：用户 / 密钥 / 渠道 / 模型四个实体的存量与健康（Sub2API DashboardStats
/// 的实体计数区 + 老 ok-api Overview 的 channels total/active/healthy）。
///
/// 纯 PG 计数，CH 未启用也照常显示——最小部署的落地页此前除了实时条什么都没有。
/// 每项可点进对应管理页；渠道项在"启用但零可用 key"时转黄、有自动停用时转红：
/// 渠道级开关绿着、key 全在冷却的渠道，在这里就该被看见。
export function InventoryStrip({ compact = false }: { compact?: boolean }) {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const q = useDashboardInventory()
  if (compact && (q.isError || !q.data)) return <section aria-label={t('admin:dashboardInventory')} className="border-t border-border/60 px-4 py-2 text-xs text-muted-foreground">
    {q.isError ? <ErrorState message={describeError(q.error)} onRetry={() => void q.refetch()} /> : <span role="status">{t('admin:invTitle')} · {t('common:loading')}</span>}
  </section>
  if (q.isError || !q.data) return <Card className="rounded-xl">
    <CardHeader><CardTitle>{t('admin:invTitle')}</CardTitle></CardHeader>
    <CardContent>{q.isError ? <ErrorState message={describeError(q.error)} onRetry={() => void q.refetch()} /> : <LoadingState />}</CardContent>
  </Card>
  const d = q.data
  const n = (v: number) => formatCount(v, locale)
  const channelTone = d.channels.auto_disabled > 0 ? 'bad' : d.channels.no_key > 0 ? 'warn' : undefined
  const channelIssues = [
    d.channels.auto_disabled > 0 ? t('admin:invChannelsAutoDisabled', { n: n(d.channels.auto_disabled) }) : '',
    d.channels.no_key > 0 ? t('admin:invChannelsNoKey', { n: n(d.channels.no_key) }) : '',
  ].filter(Boolean)
  const channelSub = channelIssues.join(' · ') || t('admin:invChannelsHealthy', { n: n(d.channels.healthy), total: n(d.channels.total) })
  const channelSummary = [
    d.channels.auto_disabled > 0 ? t('admin:dashboardDisabledCount', { n: n(d.channels.auto_disabled) }) : '',
    d.channels.no_key > 0 ? t('admin:dashboardNoKeyCount', { n: n(d.channels.no_key) }) : '',
  ].filter(Boolean).join(' · ') || t(d.channels.healthy > 0 ? 'admin:dashboardChannelsReady' : 'admin:dashboardChannelsUnavailable')
  const unpriced = d.models.total - d.models.priced

  const items = <>
        <Item
          compact={compact}
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
          compact={compact}
          icon={KeyRound}
          label={t('admin:invKeys')}
          value={n(d.api_keys.active)}
          sub={t('admin:invKeysUsed7d', { n: n(d.api_keys.used_7d) })}
          to="/admin/keys"
          summary={t('admin:dashboardKeysUsed', { n: n(d.api_keys.used_7d) })}
        />
        <Item
          compact={compact}
          icon={Server}
          label={t(compact ? 'admin:dashboardAvailableChannels' : 'admin:invChannels')}
          value={compact ? `${n(d.channels.healthy)} / ${n(d.channels.total)}` : n(d.channels.total)}
          sub={compact && channelTone ? `${t('admin:invChannelsHealthy', { n: n(d.channels.healthy), total: n(d.channels.total) })} · ${channelSub}` : channelSub}
          tone={channelTone}
          summary={channelSummary}
          to="/admin/channels"
        />
        <Item
          compact={compact}
          icon={Boxes}
          label={t('admin:invModels')}
          value={n(d.models.total)}
          sub={
            unpriced > 0
              ? t('admin:invModelsUnpriced', { n: n(unpriced) })
              : t('admin:invModelsServed', { n: n(d.models.served) })
          }
          tone={unpriced > 0 ? 'warn' : undefined}
          summary={unpriced > 0 ? t('admin:dashboardUnpricedCount', { n: n(unpriced) }) : t('admin:dashboardServedCount', { n: n(d.models.served) })}
          to="/admin/pricing"
        />
      </>
  if (compact) return <section aria-label={t('admin:dashboardInventory')} className="grid min-w-0 grid-cols-2 gap-x-2 border-t border-border/60 px-2 py-0.5 sm:grid-cols-4">{items}</section>
  return (
    <Card className="rounded-xl">
      <CardHeader className="px-4 pt-2 pb-0"><CardTitle className="text-xs text-muted-foreground">{t('admin:invTitle')}</CardTitle></CardHeader>
      <CardContent className="grid grid-cols-2 gap-x-2 gap-y-2 px-2 pt-1 pb-2">{items}</CardContent>
    </Card>
  )
}
