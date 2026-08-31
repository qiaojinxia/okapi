import { createFileRoute } from '@tanstack/react-router'
import { ChannelsPage } from '@/features/channels/ChannelsPage'

export const Route = createFileRoute('/admin/channels')({
  component: ChannelsPage,
})
