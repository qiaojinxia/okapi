import { createFileRoute } from '@tanstack/react-router'
import { PortalKeysPage } from '@/features/portal-keys/PortalKeysPage'

export const Route = createFileRoute('/portal/keys')({
  component: PortalKeysPage,
})
