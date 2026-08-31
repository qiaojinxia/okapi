import { createFileRoute } from '@tanstack/react-router'
import { RedemptionsPage } from '@/features/codes/RedemptionsPage'

export const Route = createFileRoute('/admin/codes')({
  component: RedemptionsPage,
})
