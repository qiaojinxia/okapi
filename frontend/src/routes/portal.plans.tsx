import { createFileRoute } from '@tanstack/react-router'
import { PortalPlansPage } from '@/features/subscriptions/PortalPlansPage'

export const Route = createFileRoute('/portal/plans')({
  component: PortalPlansPage,
})
