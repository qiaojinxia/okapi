import { createFileRoute } from '@tanstack/react-router'
import { RulesPage } from '@/features/rules/RulesPage'
import { pageSearch } from '@/hooks/use-pagination'

export const Route = createFileRoute('/admin/rules')({
  staticData: { fitViewport: true },
  validateSearch: pageSearch,
  component: RulesPage,
})
