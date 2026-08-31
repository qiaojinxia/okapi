import { createFileRoute } from '@tanstack/react-router'
import { PlansPage } from '@/features/plans/PlansPage'

export const Route = createFileRoute('/admin/plans')({
  component: PlansPage,
})
