import { createFileRoute } from '@tanstack/react-router'
import { RedemptionsPage } from '@/features/codes/RedemptionsPage'
import { CODE_STATUS_FILTERS, type CodesSearch } from '@/features/codes/types'
import { pageSearch } from '@/hooks/use-pagination'
import { oneOf } from '@/lib/search-params'

export const Route = createFileRoute('/admin/codes')({
  validateSearch: (search: Record<string, unknown>): CodesSearch => ({
    ...pageSearch(search),
    status: oneOf(search.status, CODE_STATUS_FILTERS),
  }),
  component: RedemptionsPage,
  staticData: { fitViewport: true },
})
