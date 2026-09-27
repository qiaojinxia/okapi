import { createFileRoute } from '@tanstack/react-router'
import { QualityPage } from '@/features/quality/QualityPage'
import { qualitySearch } from '@/features/quality/search'

export const Route = createFileRoute('/admin/quality')({
  validateSearch: qualitySearch,
  component: QualityPage,
})
