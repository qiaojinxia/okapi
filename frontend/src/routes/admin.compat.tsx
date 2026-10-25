import { createFileRoute } from '@tanstack/react-router'
import { CompatPage } from '@/features/compat/CompatPage'

export const Route = createFileRoute('/admin/compat')({
  component: CompatPage,
})
