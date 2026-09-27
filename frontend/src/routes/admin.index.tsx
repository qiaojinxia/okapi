import { createFileRoute } from '@tanstack/react-router'
import { DashboardPage } from '@/features/dashboard/DashboardPage'
import type { DashboardTrend, RankingMetric } from '@/features/dashboard/types'

function ranking(value: unknown): RankingMetric | undefined {
  return value === 'requests' || value === 'tokens' ? value : undefined
}

export const Route = createFileRoute('/admin/')({
  validateSearch: (search: Record<string, unknown>): { days?: number; scope?: 'today' | 'window'; trend?: DashboardTrend; model_rank?: RankingMetric; channel_rank?: RankingMetric } => ({
    days: [1, 7, 30].includes(Number(search.days)) ? Number(search.days) : undefined,
    scope: search.scope === 'window' ? 'window' : undefined,
    trend: search.trend === 'amount' ? 'amount' : ranking(search.trend),
    model_rank: ranking(search.model_rank),
    channel_rank: ranking(search.channel_rank),
  }),
  component: DashboardPage,
})
