import { createFileRoute } from '@tanstack/react-router'
import { RolesPage } from '@/features/roles/RolesPage'

export const Route = createFileRoute('/admin/roles')({
  component: RolesPage,
})
