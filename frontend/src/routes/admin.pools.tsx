import { createFileRoute } from '@tanstack/react-router'
import { PoolsPage } from '@/features/pools/PoolsPage'
import { pageSearch } from '@/hooks/use-pagination'

export const Route = createFileRoute('/admin/pools')({
  staticData: { fitViewport: true },
  validateSearch: pageSearch,
  component: PoolsPage,
})
