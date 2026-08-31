import { useQuery } from '@tanstack/react-query'
import { LoginForm } from '@/features/auth/LoginForm'
import { SetupWizard } from '@/features/auth/SetupWizard'
import { apiFetch } from '@/lib/api'
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
