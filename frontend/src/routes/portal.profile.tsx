import { createFileRoute } from '@tanstack/react-router'
import { ProfilePage } from '@/features/profile/ProfilePage'

export const Route = createFileRoute('/portal/profile')({ component: ProfilePage })
