import { createFileRoute } from '@tanstack/react-router'
import { ProxiesPage } from '@/features/proxies/ProxiesPage'
import { pageSearch } from '@/hooks/use-pagination'

export const Route = createFileRoute('/admin/proxies')({
  staticData: { fitViewport: true },
  validateSearch: pageSearch,
  component: ProxiesPage,
})
