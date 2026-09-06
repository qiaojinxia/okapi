import { createFileRoute } from '@tanstack/react-router'
import { PortalKeysPage } from '@/features/portal-keys/PortalKeysPage'
import { pageSearch } from '@/hooks/use-pagination'

export const Route = createFileRoute('/portal/keys')({
  validateSearch: pageSearch,
  component: PortalKeysPage,
})
