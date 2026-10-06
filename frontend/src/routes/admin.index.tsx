import { createFileRoute } from '@tanstack/react-router'
import { DashboardPage } from '@/features/dashboard/DashboardPage'
import type { DashboardTrend, DistributionView, RankingMetric } from '@/features/dashboard/types'
import { dashboardPeriodSearch } from '@/features/dashboard/period'

function ranking(value: unknown): RankingMetric | undefined {
  return value === 'requests' || value === 'tokens' ? value : undefined
}

export const Route = createFileRoute('/admin/')({
  staticData: { fitViewport: true },
  validateSearch: (search: Record<string, unknown>): { days?: number; start_date?: string; end_date?: string; trend?: DashboardTrend; distribution?: DistributionView; model_rank?: RankingMetric; channel_rank?: RankingMetric } => ({
    ...dashboardPeriodSearch(search),
    trend: search.trend === 'amount' ? 'amount' : ranking(search.trend),
    distribution: search.distribution === 'channel' || search.distribution === 'tokens' ? search.distribution : undefined,
    model_rank: ranking(search.model_rank),
    channel_rank: ranking(search.channel_rank),
  }),
  component: DashboardPage,
})
