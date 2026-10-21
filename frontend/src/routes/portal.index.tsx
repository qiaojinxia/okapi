import { createFileRoute } from '@tanstack/react-router'
import { PortalOverviewPage } from '@/features/portal-overview/PortalOverviewPage'
import { overviewSearch } from '@/features/portal-overview/search'

export const Route = createFileRoute('/portal/')({
  validateSearch: overviewSearch,
  // 总览撑满视口：趋势与模型排行吃掉页头、指标之外的剩余高度（小视口在页内滚动）。
  staticData: { fitViewport: true },
  component: PortalOverviewPage,
})
