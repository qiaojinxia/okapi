import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { PageHeader } from '@/components/ui/page'
import { Tabs } from '@/components/ui/tabs'
import { DaysPicker } from '@/features/stats/DaysPicker'
import { LeaderboardCard } from '@/features/stats/LeaderboardCard'
import { RevenueCard } from '@/features/stats/RevenueCard'

const TABS = ['revenue', 'leaderboard'] as const
type Tab = (typeof TABS)[number]

/// 经营报表：收入与让利（含资金流入、按分组）/ 用户消耗排行。
///
/// 从旧统计页拆出来的第三个问题——"赚不赚钱、钱从哪来、谁贡献的"。读者是站长
/// 本人（财务视角），与运维视角的"服务质量"、产品视角的"用量分析"分开，
/// 三种人各进各的页，不用在七个页签里找自己那一个。
export function RevenuePage() {
  const { t } = useTranslation()
  const [days, setDays] = useState(7)
  const [tab, setTab] = useState<Tab>('revenue')

  const labels: Record<Tab, string> = {
    revenue: t('admin:statRevenue'),
    leaderboard: t('admin:leaderboard'),
  }

  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        title={t('analytics:revenueTitle')}
        description={t('analytics:revenueDesc')}
        action={<DaysPicker days={days} onPick={setDays} />}
      />
      <Tabs
        items={TABS.map((id) => ({ id, label: labels[id] }))}
        active={tab}
        onChange={(id) => setTab(id as Tab)}
      />
      {tab === 'revenue' && <RevenueCard days={days} />}
      {tab === 'leaderboard' && <LeaderboardCard days={days} />}
    </div>
  )
}
