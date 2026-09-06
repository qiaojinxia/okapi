import { createFileRoute } from '@tanstack/react-router'
import { ForgotPasswordForm } from '@/features/auth/PasswordRecovery'

/// 找回密码：从登录页"忘记密码"进来时把已填的邮箱带过来，免得再敲一遍。
export const Route = createFileRoute('/forgot-password')({
  validateSearch: (search: Record<string, unknown>): { email?: string } => ({
    email: typeof search.email === 'string' && search.email.length > 0 ? search.email : undefined,
  }),
  component: () => {
    const { email } = Route.useSearch()
    return <ForgotPasswordForm initialEmail={email} />
  },
})
