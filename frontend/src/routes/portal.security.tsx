import { createFileRoute } from '@tanstack/react-router'
import { SecurityPage } from '@/features/security/SecurityPage'

export const Route = createFileRoute('/portal/security')({
  staticData: { fitViewport: true },
  component: SecurityPage,
})
