import { createFileRoute } from '@tanstack/react-router'
import { PublicPricingPage } from '@/features/public-pricing/PublicPricingPage'

export const Route = createFileRoute('/pricing')({
  component: PublicPricingPage,
})
