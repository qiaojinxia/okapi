import { createFileRoute } from '@tanstack/react-router'
import { ResetPasswordForm } from '@/features/auth/PasswordRecovery'

/// 邮件里的重置链接落地页：`/reset-password?token=…`（链接由后端 site_url 拼出，§11.27）。
export const Route = createFileRoute('/reset-password')({
  validateSearch: (search: Record<string, unknown>): { token: string } => ({
    token: typeof search.token === 'string' ? search.token : '',
  }),
  component: () => {
    const { token } = Route.useSearch()
    return <ResetPasswordForm token={token} />
  },
})
