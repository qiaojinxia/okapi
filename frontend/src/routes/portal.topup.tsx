import { createFileRoute } from '@tanstack/react-router'
import { TopupPage } from '@/features/topup/TopupPage'

export const Route = createFileRoute('/portal/topup')({
  component: TopupPage,
})
