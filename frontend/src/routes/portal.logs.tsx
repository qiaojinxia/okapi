import { createFileRoute } from '@tanstack/react-router'
import { LogsPage } from '@/features/logs/LogsPage'
import { portalLogSearch } from '@/features/logs/search'

export const Route = createFileRoute('/portal/logs')({
  staticData: { fitViewport: true },
  validateSearch: portalLogSearch,
  component: LogsPage,
})
