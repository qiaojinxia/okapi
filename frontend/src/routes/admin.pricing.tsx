import { createFileRoute } from '@tanstack/react-router'
import { ModelPricingPage } from '@/features/models/ModelPricingPage'

export const Route = createFileRoute('/admin/pricing')({
  component: ModelPricingPage,
})
