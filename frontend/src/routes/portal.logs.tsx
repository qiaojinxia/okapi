import { createFileRoute } from '@tanstack/react-router'
import { LogsPage } from '@/features/logs/LogsPage'

export const Route = createFileRoute('/portal/logs')({
  component: LogsPage,
})
