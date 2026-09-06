import { createFileRoute } from '@tanstack/react-router'
import { PlansPage } from '@/features/plans/PlansPage'
import { pageSearch } from '@/hooks/use-pagination'

export const Route = createFileRoute('/admin/plans')({
  validateSearch: pageSearch,
  component: PlansPage,
})
