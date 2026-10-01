import { createFileRoute } from '@tanstack/react-router'
import { PlaygroundPage } from '@/features/playground/PlaygroundPage'

export const Route = createFileRoute('/portal/playground')({
  staticData: { fitViewport: true },
  component: PlaygroundPage,
})
