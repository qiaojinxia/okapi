import { Link, useNavigate } from '@tanstack/react-router'
import { ArrowRight, MailCheck } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Alert } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Field } from '@/components/ui/field'
import { Input } from '@/components/ui/input'
import { PasswordInput } from '@/components/ui/password-input'
import { AuthLayout } from '@/features/auth/AuthLayout'
import { describeAuthError } from '@/features/auth/LoginForm'
import { apiFetch } from '@/lib/api'

/// 找回密码第一步：填邮箱 → 后端寄重置链接（IMPLEMENTATION §11.27）。
/// 后端无论邮箱是否存在都回 ok（防枚举），所以成功态文案只能说"如果这个邮箱有账号就会收到信"。
export function ForgotPasswordForm({ initialEmail }: { initialEmail?: string }) {
  const { t, i18n } = useTranslation()
  const [email, setEmail] = useState(initialEmail ?? '')
  const [sent, setSent] = useState(false)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const submit = async () => {
    setBusy(true)
    setError(null)
    try {
      await apiFetch('/auth/password/forgot', {
        method: 'POST',
        body: { email: email.trim(), lang: i18n.language },
      })
      setSent(true)
    } catch (err) {
      setError(describeAuthError(err))
    } finally {
      setBusy(false)
    }
  }

  return (
    <AuthLayout title={t('auth:forgotTitle')} subtitle={t('auth:forgotSubtitle')} footer={<BackToLogin />}>
      <div className="flex flex-col gap-5 rounded-xl border border-border bg-card p-6 shadow-card">
        {sent ? (
          <Alert tone="success">
            <span className="flex items-start gap-2">
              <MailCheck className="mt-0.5 h-4 w-4 shrink-0" />
              {t('auth:forgotSent', { email: email.trim() })}
            </span>
          </Alert>
        ) : (
          <form
            className="flex flex-col gap-4"
            onSubmit={(e) => {
              e.preventDefault()
              void submit()
            }}
          >
            <Field label={t('auth:email')} htmlFor="forgot-email">
              <Input
                id="forgot-email"
                type="email"
                autoComplete="email"
                autoFocus
                value={email}
                onChange={(e) => setEmail(e.target.value)}
              />
            </Field>
            {error && <Alert tone="destructive">{error}</Alert>}
            <Button type="submit" size="lg" loading={busy} disabled={!email.includes('@')}>
              {t('auth:sendResetLink')}
              <ArrowRight className="h-4 w-4" />
            </Button>
          </form>
        )}
      </div>
    </AuthLayout>
  )
}

/// 找回密码第二步：邮件链接 `/reset-password?token=…` 落地，设新密码。
/// token 一次性、30 分钟有效；错 / 过期后端回 400 reset_token_invalid，提示重新申请。
export function ResetPasswordForm({ token }: { token: string }) {
  const { t } = useTranslation()
  const navigate = useNavigate()
  const [password, setPassword] = useState('')
  const [confirm, setConfirm] = useState('')
  const [busy, setBusy] = useState(false)
  const [done, setDone] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const mismatch = confirm.length > 0 && confirm !== password

  const submit = async () => {
    setBusy(true)
    setError(null)
    try {
      await apiFetch('/auth/password/reset', { method: 'POST', body: { token, password } })
      setDone(true)
    } catch (err) {
      setError(describeAuthError(err))
    } finally {
      setBusy(false)
    }
  }

  return (
    <AuthLayout title={t('auth:resetTitle')} subtitle={t('auth:resetSubtitle')} footer={<BackToLogin />}>
      <div className="flex flex-col gap-5 rounded-xl border border-border bg-card p-6 shadow-card">
        {token.length === 0 ? (
          <Alert tone="warning">{t('auth:resetTokenInvalid')}</Alert>
        ) : done ? (
          <>
            <Alert tone="success">{t('auth:resetDone')}</Alert>
            <Button size="lg" onClick={() => void navigate({ to: '/' })}>
              {t('auth:backToLogin')}
              <ArrowRight className="h-4 w-4" />
            </Button>
          </>
        ) : (
          <form
            className="flex flex-col gap-4"
            onSubmit={(e) => {
              e.preventDefault()
              void submit()
            }}
          >
            <Field label={t('auth:passwordMin')} htmlFor="reset-password">
              <PasswordInput
                id="reset-password"
                autoComplete="new-password"
                autoFocus
                value={password}
                onChange={(e) => setPassword(e.target.value)}
              />
            </Field>
            <Field
              label={t('auth:confirmPassword')}
              htmlFor="reset-confirm"
              error={mismatch ? t('auth:passwordMismatch') : null}
            >
              <PasswordInput
                id="reset-confirm"
                autoComplete="new-password"
                value={confirm}
                onChange={(e) => setConfirm(e.target.value)}
              />
            </Field>
            {error && <Alert tone="destructive">{error}</Alert>}
            <Button
              type="submit"
              size="lg"
              loading={busy}
              disabled={password.length < 8 || confirm !== password}
            >
              {t('auth:resetSubmit')}
              <ArrowRight className="h-4 w-4" />
            </Button>
          </form>
        )}
      </div>
    </AuthLayout>
  )
}

function BackToLogin() {
  const { t } = useTranslation()
  return (
    <Link to="/" className="underline decoration-dotted underline-offset-4 hover:text-foreground">
      {t('auth:backToLogin')}
    </Link>
  )
}
