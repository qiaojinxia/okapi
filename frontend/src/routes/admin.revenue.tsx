import { createFileRoute } from '@tanstack/react-router'
import { RevenuePage } from '@/features/revenue/RevenuePage'

export const Route = createFileRoute('/admin/revenue')({
  component: RevenuePage,
})
