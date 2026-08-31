import { createFileRoute } from '@tanstack/react-router'
import { PortalOverviewPage } from '@/features/portal-overview/PortalOverviewPage'

export const Route = createFileRoute('/portal/')({
  component: PortalOverviewPage,
})
