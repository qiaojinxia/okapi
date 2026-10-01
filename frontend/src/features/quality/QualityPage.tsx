import { getRouteApi, Link } from '@tanstack/react-router'
import { HeartPulse } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { PageHeader } from '@/components/ui/page'
import { Tabs } from '@/components/ui/tabs'
import { ChannelHealthCard } from '@/features/stats/ChannelHealthCard'
import { ClientsCard } from '@/features/stats/ClientsCard'
import { DaysPicker } from '@/features/stats/DaysPicker'
import { ErrorBreakdownCard } from '@/features/stats/ErrorBreakdownCard'
import { ModelLatencyCard } from '@/features/stats/ModelLatencyCard'
import { QualityTrend } from './QualityTrend'
import { QUALITY_TABS } from './search'
import type { QualityTab } from './search'
import { ADVANCED_KEYS } from '@/features/analytics/advanced-search'

const routeApi = getRouteApi('/admin/quality')

/// 服务质量：渠道健康 / 模型时延 / 错误分布 / 客户端分布。
///
/// 从旧统计页拆出来的第二个问题——"服务得好不好"。读者是运维：看的是错误率、
/// 分位时延、切换率，不是钱。页签顺序照排障动线：先看哪条路坏了（渠道）→
/// 哪个模型慢（模型）→ 坏在什么错（错误码）→ 谁在打（客户端）。
export function QualityPage() {
  const { t } = useTranslation()
  const search = routeApi.useSearch()
  const { days = 7, tab = 'trend' } = search
  const navigate = routeApi.useNavigate()
  const customRange = !!search.start_date || !!search.end_date
  const savedFilters = ADVANCED_KEYS.some((key) => search[key] !== undefined && !(Array.isArray(search[key]) && !search[key]?.length))

  const labels: Record<QualityTab, string> = {
    trend: t('charts:qualityTrend'),
    channels: t('admin:statChannels'),
    models: t('admin:statModels'),
    errors: t('admin:statErrors'),
    clients: t('admin:statClients'),
  }

  return (
    <div className="flex min-w-0 flex-col gap-3">
      <PageHeader
        icon={HeartPulse}
        title={t('analytics:qualityTitle')}
        description={t('analytics:qualityDesc')}
        action={<DaysPicker days={tab === 'trend' && customRange ? 0 : days} onPick={(value) => void navigate({ search: (prev) => ({ ...prev, days: value, start_date: undefined, end_date: undefined, page: undefined }) })} />}
      />
      <Tabs
        id="quality-tabs"
        ariaLabel={t('analytics:qualityTitle')}
        items={QUALITY_TABS.map((id) => ({ id, label: labels[id], panelId: 'quality-panel' }))}
        active={tab}
        onChange={(id) => void navigate({ search: (prev) => ({ ...prev, tab: id as QualityTab, page: undefined }) })}
      />
      {tab !== 'trend' && savedFilters && <div role="note" className="flex flex-wrap items-center justify-between gap-2 rounded-xl border border-border bg-muted/30 px-3 py-2 text-xs text-muted-foreground">
        <p className="min-w-0 flex-1">{t('analytics:qualitySavedFilters', { days })}</p>
        <Link to="/admin/quality" search={{ ...search, tab: 'trend' }} className="inline-flex min-h-9 items-center rounded px-2 text-primary outline-none hover:underline focus-visible:ring-2 focus-visible:ring-primary/40">{t('analytics:qualityReturnTrend')}</Link>
      </div>}
      <div id="quality-panel" role="tabpanel" aria-labelledby={`quality-tabs-${tab}`} className="min-w-0">
        {tab === 'trend' && <QualityTrend search={search} onChange={(next) => void navigate({ search: next })} />}
        {tab === 'channels' && <ChannelHealthCard days={days} />}
        {tab === 'models' && <ModelLatencyCard days={days} />}
        {tab === 'errors' && <ErrorBreakdownCard days={days} />}
        {tab === 'clients' && <ClientsCard days={days} />}
      </div>
    </div>
  )
}
