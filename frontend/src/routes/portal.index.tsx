import { createFileRoute } from '@tanstack/react-router'
import { PortalOverviewPage } from '@/features/portal-overview/PortalOverviewPage'
import { overviewSearch } from '@/features/portal-overview/search'

export const Route = createFileRoute('/portal/')({
  validateSearch: overviewSearch,
  component: PortalOverviewPage,
})
