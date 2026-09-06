import { createFileRoute } from '@tanstack/react-router'
import { ChannelsPage } from '@/features/channels/ChannelsPage'
import { type ChannelsSearch, PROVIDERS } from '@/features/channels/types'
import { pageSearch } from '@/hooks/use-pagination'
import { oneOf, text } from '@/lib/search-params'

export const Route = createFileRoute('/admin/channels')({
  validateSearch: (search: Record<string, unknown>): ChannelsSearch => ({
    ...pageSearch(search),
    q: text(search.q),
    provider: oneOf(search.provider, PROVIDERS),
  }),
  component: ChannelsPage,
})
