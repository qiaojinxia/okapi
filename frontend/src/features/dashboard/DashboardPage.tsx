import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { PageHeader } from '@/components/ui/page'
import { AttentionCard } from '@/features/dashboard/AttentionCard'
import { InventoryStrip } from '@/features/dashboard/InventoryStrip'
import { KpiCards } from '@/features/dashboard/KpiCards'
import { RealtimeCard } from '@/features/dashboard/RealtimeCard'
import { TrendCard } from '@/features/dashboard/TrendCard'
import { DaysPicker } from '@/features/stats/DaysPicker'

/// 管理总览。
///
/// 版面顺序照运维视线走：先"此刻"（实时条，秒级）→ "今天"（KPI，日聚合）→
/// 再"该做什么"（待办）→ 最后"趋势如何"（图表）。待办放在图表之前是刻意的：
/// 图表要盯着看才有信息，待办是一眼就能行动的东西，落地页应优先给能立刻处理的。
export function DashboardPage() {
  const { t } = useTranslation()
  const [days, setDays] = useState(7)

  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        title={t('admin:overview')}
        description={t('admin:overviewDesc')}
        action={<DaysPicker days={days} onPick={setDays} />}
      />
      <RealtimeCard />
      <KpiCards days={days} />
      {/* 存量与健康（PG）：流量 KPI 之下、待办之上——"有多少东西"是读待办的前提 */}
      <InventoryStrip />
      <div className="grid grid-cols-1 gap-4 xl:grid-cols-3">
        <div className="xl:col-span-2">
          <TrendCard days={days} />
        </div>
        <AttentionCard days={days} />
      </div>
    </div>
  )
}
