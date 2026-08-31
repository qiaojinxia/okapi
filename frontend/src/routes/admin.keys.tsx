import { createFileRoute } from '@tanstack/react-router'
import { AdminKeysPage } from '@/features/keys/AdminKeysPage'

export const Route = createFileRoute('/admin/keys')({
  component: AdminKeysPage,
})
