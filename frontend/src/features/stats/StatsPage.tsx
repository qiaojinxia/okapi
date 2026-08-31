import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { ChannelHealthCard } from '@/features/stats/ChannelHealthCard'
import { DaysPicker } from '@/features/stats/DaysPicker'
import { LeaderboardCard } from '@/features/stats/LeaderboardCard'
import { ModelLatencyCard } from '@/features/stats/ModelLatencyCard'
import { OverviewCard } from '@/features/stats/OverviewCard'
import { PageHeader } from '@/components/ui/page'
import { RevenueCard } from '@/features/stats/RevenueCard'

export function StatsPage() {
  const { t } = useTranslation()
  const [days, setDays] = useState(7)
  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        title={t('admin:statNav')}
        description={t('admin:statsDesc')}
        action={<DaysPicker days={days} onPick={setDays} />}
      />
      <OverviewCard days={days} />
      <ChannelHealthCard days={days} />
      <ModelLatencyCard days={days} />
      <RevenueCard days={days} />
      <LeaderboardCard days={days} />
    </div>
  )
}
