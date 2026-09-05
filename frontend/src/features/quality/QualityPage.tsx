import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { PageHeader } from '@/components/ui/page'
import { Tabs } from '@/components/ui/tabs'
import { ChannelHealthCard } from '@/features/stats/ChannelHealthCard'
import { ClientsCard } from '@/features/stats/ClientsCard'
import { DaysPicker } from '@/features/stats/DaysPicker'
import { ErrorBreakdownCard } from '@/features/stats/ErrorBreakdownCard'
import { ModelLatencyCard } from '@/features/stats/ModelLatencyCard'
import { QualityTrend } from './QualityTrend'

const TABS = ['trend', 'channels', 'models', 'errors', 'clients'] as const
type Tab = (typeof TABS)[number]

/// 服务质量：渠道健康 / 模型时延 / 错误分布 / 客户端分布。
///
/// 从旧统计页拆出来的第二个问题——"服务得好不好"。读者是运维：看的是错误率、
/// 分位时延、切换率，不是钱。页签顺序照排障动线：先看哪条路坏了（渠道）→
/// 哪个模型慢（模型）→ 坏在什么错（错误码）→ 谁在打（客户端）。
export function QualityPage() {
  const { t } = useTranslation()
  const [days, setDays] = useState(7)
  const [tab, setTab] = useState<Tab>('trend')

  const labels: Record<Tab, string> = {
    trend: t('charts:qualityTrend'),
    channels: t('admin:statChannels'),
    models: t('admin:statModels'),
    errors: t('admin:statErrors'),
    clients: t('admin:statClients'),
  }

  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        title={t('analytics:qualityTitle')}
        description={t('analytics:qualityDesc')}
        action={<DaysPicker days={days} onPick={setDays} />}
      />
      <Tabs
        items={TABS.map((id) => ({ id, label: labels[id] }))}
        active={tab}
        onChange={(id) => setTab(id as Tab)}
      />
      {tab === 'trend' && <QualityTrend key={days} days={days} />}
      {tab === 'channels' && <ChannelHealthCard days={days} />}
      {tab === 'models' && <ModelLatencyCard days={days} />}
      {tab === 'errors' && <ErrorBreakdownCard days={days} />}
      {tab === 'clients' && <ClientsCard days={days} />}
    </div>
  )
}
