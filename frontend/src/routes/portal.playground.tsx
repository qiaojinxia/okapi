import { createFileRoute } from '@tanstack/react-router'
import { PlaygroundPage } from '@/features/playground/PlaygroundPage'

export const Route = createFileRoute('/portal/playground')({
  component: PlaygroundPage,
})
