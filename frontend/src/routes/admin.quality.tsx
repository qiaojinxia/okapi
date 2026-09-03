import { createFileRoute } from '@tanstack/react-router'
import { QualityPage } from '@/features/quality/QualityPage'

export const Route = createFileRoute('/admin/quality')({
  component: QualityPage,
})
