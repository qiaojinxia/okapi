import { createFileRoute } from '@tanstack/react-router'
import { MonitorPage } from '@/features/monitor/MonitorPage'

export const Route = createFileRoute('/admin/monitor')({
  component: MonitorPage,
})
