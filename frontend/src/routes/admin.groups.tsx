import { createFileRoute } from '@tanstack/react-router'
import { GroupsPage } from '@/features/groups/GroupsPage'
import { pageSearch } from '@/hooks/use-pagination'

export const Route = createFileRoute('/admin/groups')({
  validateSearch: pageSearch,
  component: GroupsPage,
  staticData: { fitViewport: true },
})
