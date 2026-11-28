import { createFileRoute } from '@tanstack/react-router'
import { ChannelsPage } from '@/features/channels/ChannelsPage'
import {
  CHANNEL_STATUS_FILTERS,
  CHANNEL_TABS,
  type ChannelsSearch,
  PROVIDERS,
} from '@/features/channels/types'
import { pageSearch } from '@/hooks/use-pagination'
import { oneOf, text } from '@/lib/search-params'

export const Route = createFileRoute('/admin/channels')({
  staticData: { fitViewport: true },
  validateSearch: (search: Record<string, unknown>): ChannelsSearch => ({
    ...pageSearch(search),
    tab: oneOf(search.tab, CHANNEL_TABS),
    q: text(search.q),
    provider: oneOf(search.provider, PROVIDERS),
    status: oneOf(search.status, CHANNEL_STATUS_FILTERS),
  }),
  component: ChannelsPage,
})
