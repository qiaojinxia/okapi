import { Link } from '@tanstack/react-router'
import { TriangleAlert } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { Skeleton } from '@/components/ui/skeleton'
import { ErrorState } from '@/components/ui/state'
import { Tooltip } from '@/components/ui/tooltip'
import { describeError } from '@/lib/i18n'
import { formatCount } from '@/lib/money'
import { cn } from '@/lib/utils'
import { useDashboardInventory } from './data'

function Cell({ label, value, sub, summary, to, tone }: {
  label: string
  value: string
  /// 完整说明：悬停提示和读屏名称用。
  sub: string
  /// 单元格里的一句话。
  summary: string
  to: string
  tone?: 'warn' | 'bad'
}) {
  return <Tooltip content={sub} className="min-w-0">
    <Link to={to} aria-label={`${label} ${value} · ${sub}`} className="flex min-w-0 flex-1 flex-col bg-card px-2.5 py-1.5 outline-none hover:bg-muted/50 focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-primary/40">
      <span className="text-xs text-muted-foreground">{label}</span>
      <span className="flex flex-wrap items-baseline gap-x-1.5 text-[15px] font-semibold tabular-nums">
        {value}
        <span className={cn('inline-flex min-w-0 items-center gap-1 text-xs font-normal', tone === 'bad' ? 'text-destructive' : tone === 'warn' ? 'text-warning' : 'text-muted-foreground/80')}>
          {tone && <TriangleAlert aria-hidden className="h-3 w-3 shrink-0" />}
          <span className="break-words">{summary}</span>
        </span>
      </span>
    </Link>
  </Tooltip>
}

/// 站点规模速览：用户 / 活跃密钥 / 可用渠道 / 模型四个实体的存量与健康，2×2 贴在用量分布卡底部。
///
/// 纯 PG 计数，CH 未启用也照常显示。每格可点进对应管理页；渠道在“启用但零可用 key”时转黄、
/// 有自动停用时转红，模型有未定价的转黄——渠道级开关绿着、key 全在冷却的渠道，在这里就该被看见。
export function InventoryGrid() {
  const { t, i18n } = useTranslation()
  const locale = i18n.language
  const q = useDashboardInventory()
  const frame = 'grid grid-cols-2 gap-px overflow-hidden rounded-[10px] border border-border bg-border'
  if (q.isError) return <section aria-label={t('admin:dashboardInventory')}><ErrorState message={describeError(q.error)} onRetry={() => void q.refetch()} /></section>
  if (!q.data) return <section aria-label={t('admin:dashboardInventory')} aria-busy className={frame}>
    {[0, 1, 2, 3].map((i) => <div key={i} className="flex flex-col gap-1 bg-card px-2.5 py-2"><Skeleton className="h-3 w-10" /><Skeleton className="h-4 w-20" /></div>)}
    <span role="status" className="sr-only">{t('admin:invTitle')} · {t('common:loading')}</span>
  </section>
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
  return <section aria-label={t('admin:dashboardInventory')} className={frame}>
    <Cell label={t('admin:invUsers')} value={n(d.users.total)} to="/admin/users"
      sub={d.users.new_today > 0 ? t('admin:invUsersNewToday', { n: n(d.users.new_today) }) : t('admin:invUsersNew7d', { n: n(d.users.new_7d) })}
      summary={d.users.new_today > 0 ? t('admin:invUsersNewToday', { n: n(d.users.new_today) }) : t('admin:invUsersNew7d', { n: n(d.users.new_7d) })} />
    <Cell label={t('admin:invKeys')} value={n(d.api_keys.active)} to="/admin/keys"
      sub={t('admin:invKeysUsed7d', { n: n(d.api_keys.used_7d) })} summary={t('admin:dashboardKeysUsed', { n: n(d.api_keys.used_7d) })} />
    <Cell label={t('admin:dashboardAvailableChannels')} value={`${n(d.channels.healthy)} / ${n(d.channels.total)}`} to="/admin/channels" tone={channelTone}
      sub={channelTone ? `${t('admin:invChannelsHealthy', { n: n(d.channels.healthy), total: n(d.channels.total) })} · ${channelSub}` : channelSub} summary={channelSummary} />
    <Cell label={t('admin:invModels')} value={n(d.models.total)} to="/admin/pricing" tone={unpriced > 0 ? 'warn' : undefined}
      sub={unpriced > 0 ? t('admin:invModelsUnpriced', { n: n(unpriced) }) : t('admin:invModelsServed', { n: n(d.models.served) })}
      summary={unpriced > 0 ? t('admin:dashboardUnpricedCount', { n: n(unpriced) }) : t('admin:dashboardServedCount', { n: n(d.models.served) })} />
  </section>
}
