import { createFileRoute, redirect } from '@tanstack/react-router'
import { AuthEntryError, AuthEntryPage } from '@/features/auth/AuthEntryPage'
import { LoadingState } from '@/components/ui/state'
import { ApiError, apiFetch, clearKey, getKey } from '@/lib/api'

export const Route = createFileRoute('/')({
  beforeLoad: async ({ location }) => {
    // OAuth must exchange the new web session before checking an older local key.
    if (new URLSearchParams(location.searchStr).get('oauth') === 'done') return
    const key = getKey()
    if (!key?.trim()) {
      if (key !== null) clearKey()
      return
    }
    try {
      await apiFetch('/api/me', { key })
    } catch (error) {
      // Only an authentication rejection invalidates the saved credential.
      // Network/5xx failures must not log the user out or create redirect loops.
      if (error instanceof ApiError && error.status === 401) {
        if (getKey() === key) clearKey()
        return
      }
      throw error
    }
    if (getKey() === key) throw redirect({ to: '/portal', replace: true })
  },
  pendingMs: 0,
  pendingMinMs: 0,
  pendingComponent: () => <LoadingState className="min-h-dvh" />,
  errorComponent: AuthEntryError,
  component: AuthEntryPage,
})
