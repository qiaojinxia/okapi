import { createFileRoute } from '@tanstack/react-router'
import { OpsPage } from '@/features/ops/OpsPage'

export const Route = createFileRoute('/admin/ops')({
  component: OpsPage,
})
