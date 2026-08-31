import { createFileRoute } from '@tanstack/react-router'
import { AuthEntryPage } from '@/features/auth/AuthEntryPage'

export const Route = createFileRoute('/')({
  component: AuthEntryPage,
})
