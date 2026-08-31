import { createFileRoute } from '@tanstack/react-router'
import { RulesPage } from '@/features/rules/RulesPage'

export const Route = createFileRoute('/admin/rules')({
  component: RulesPage,
})
