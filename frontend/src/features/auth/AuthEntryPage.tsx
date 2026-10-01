import { useQuery } from '@tanstack/react-query'
import { useRouter } from '@tanstack/react-router'
import type { ErrorComponentProps } from '@tanstack/react-router'
import { useTranslation } from 'react-i18next'
import { AuthLayout } from '@/features/auth/AuthLayout'
import { LoginForm } from '@/features/auth/LoginForm'
import { SetupWizard } from '@/features/auth/SetupWizard'
import { ErrorState } from '@/components/ui/state'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { qk } from '@/lib/query-keys'

/// 入口页：首次部署（无超管）走安装向导，否则走登录。
export function AuthEntryPage() {
  const setup = useQuery({
    queryKey: qk.setupStatus,
    queryFn: () => apiFetch<{ needs_setup: boolean }>('/api/setup/status'),
    retry: 0,
  })
  if (setup.data?.needs_setup) {
    return <SetupWizard />
  }
  return <LoginForm />
}

/// A failed login check is recoverable; never mistake an outage for a sign-out.
export function AuthEntryError({ error }: ErrorComponentProps) {
  const { t } = useTranslation()
  const router = useRouter()
  return <AuthLayout title={t('auth:welcomeBack')} subtitle={t('auth:signInSubtitle')}>
    <ErrorState message={describeError(error)} onRetry={() => { void router.invalidate() }} />
  </AuthLayout>
}
