import { createFileRoute } from '@tanstack/react-router'
import { PoolsPage } from '@/features/pools/PoolsPage'

export const Route = createFileRoute('/admin/pools')({
  component: PoolsPage,
})
