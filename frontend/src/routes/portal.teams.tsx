import { createFileRoute } from '@tanstack/react-router'
import { TeamsPage } from '@/features/teams/TeamsPage'
import { pageSearch } from '@/hooks/use-pagination'

export const Route = createFileRoute('/portal/teams')({
  validateSearch: pageSearch,
  component: TeamsPage,
})
