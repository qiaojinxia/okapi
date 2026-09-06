import { createFileRoute } from '@tanstack/react-router'
import { RolesPage } from '@/features/roles/RolesPage'
import { pageSearch } from '@/hooks/use-pagination'

export const Route = createFileRoute('/admin/roles')({
  validateSearch: pageSearch,
  component: RolesPage,
})
