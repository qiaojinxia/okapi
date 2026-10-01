import { createFileRoute } from '@tanstack/react-router'
import { ProfilePage } from '@/features/profile/ProfilePage'

export const Route = createFileRoute('/portal/profile')({
  validateSearch: (search: Record<string, unknown>): { tab?: 'info' | 'signins' } => ({ tab: search.tab === 'info' || search.tab === 'signins' ? search.tab : undefined }),
  component: ProfilePage,
})
