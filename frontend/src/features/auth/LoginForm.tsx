import { Link, useNavigate } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'
import { ArrowRight, KeyRound, Mail, UserPlus } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { ApiError, apiFetch, setKey } from '@/lib/api'
import { Alert } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Field } from '@/components/ui/field'
import { Input } from '@/components/ui/input'
import { PasswordInput } from '@/components/ui/password-input'
import { Segmented } from '@/components/ui/segmented'
import { AuthLayout } from '@/features/auth/AuthLayout'
import { describeError } from '@/lib/i18n'
import { formatMoney } from '@/lib/money'
import { qk } from '@/lib/query-keys'

/// OAuth 回调着陆（?oauth=done）：会话已建，兑 key 进门户。
export function useOauthLanding(onError: (msg: string) => void) {
  const navigate = useNavigate()
  const landed = new URLSearchParams(window.location.search).get('oauth') === 'done'
  const exchange = useQuery({
    queryKey: qk.oauthExchange,
    queryFn: async () => {
      const resp = await apiFetch<{ api_key: string }>('/auth/keys', {
        method: 'POST',
        body: { name: 'oauth' },
      })
      setKey(resp.api_key)
      await navigate({ to: '/portal' })
      return true
    },
    enabled: landed,
    retry: 0,
  })
  if (exchange.isError && landed) {
    onError(describeError(exchange.error))
  }
  return landed && exchange.isPending
}

export type LoginTab = 'password' | 'key' | 'register'

/// 登录 / 注册 / API Key 三种入口共用一张卡。
///
/// 三种方式用分段选择器切换（此前是三个实心按钮，看起来像三个动作而不是三个选项）；
/// 每种方式是一个真正的 `<form>`：回车提交、浏览器自动填充与密码管理器都能正常工作。
export function LoginForm() {
  const { t, i18n } = useTranslation()
  const navigate = useNavigate()
  const [tab, setTab] = useState<LoginTab>('password')
  const [key, setKeyInput] = useState('')
  const [form, setForm] = useState({ email: '', password: '', totp: '' })
  const [regForm, setRegForm] = useState({ email: '', username: '', password: '' })
  const [totpNeeded, setTotpNeeded] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  // 邀请链接落地（/?aff=code）：注册请求隐式携带；邀请制下没带链接的人可手填
  const affFromUrl = new URLSearchParams(window.location.search).get('aff')
  const [affInput, setAffInput] = useState('')
  const affCode = affFromUrl ?? (affInput.trim() || null)

  // 注册策略（§11.16）：关闭时不摆一个必然失败的表单；邀请制把邀请码变必填；有赠送写出来
  const regPolicy = useQuery({
    queryKey: qk.registrationPolicy,
    queryFn: () =>
      apiFetch<{
        mode: 'open' | 'invite_only' | 'closed'
        new_user_credit_micro: number
        invitee_credit_micro: number
        allowed_domains: string[]
      }>('/api/registration'),
    retry: 0,
    staleTime: 60_000,
  })
  const regMode = regPolicy.data?.mode ?? 'open'
  const giftMicro =
    (regPolicy.data?.new_user_credit_micro ?? 0) +
    (affCode ? (regPolicy.data?.invitee_credit_micro ?? 0) : 0)

  const oauthPending = useOauthLanding(setError)
  const providers = useQuery({
    queryKey: qk.oauthProviders,
    queryFn: () => apiFetch<{ providers: string[] }>('/auth/oauth-providers'),
    retry: 0,
  })

  const switchTab = (next: LoginTab) => {
    setTab(next)
    setError(null)
  }

  const submitKey = async () => {
    if (!key.trim()) return
    setBusy(true)
    setError(null)
    try {
      await apiFetch('/api/me', { key: key.trim() })
      setKey(key.trim())
      await navigate({ to: '/portal' })
    } catch (err) {
      setError(describeError(err))
    } finally {
      setBusy(false)
    }
  }

  const submitRegister = async () => {
    setBusy(true)
    setError(null)
    try {
      await apiFetch('/auth/register', {
        method: 'POST',
        body: {
          email: regForm.email.trim(),
          username: regForm.username.trim(),
          password: regForm.password,
          aff_code: affCode ?? undefined,
        },
      })
      // 注册即登录：建会话 → 兑 key 进门户（key 单轨）
      await apiFetch('/auth/login', {
        method: 'POST',
        body: { email: regForm.email.trim(), password: regForm.password },
      })
      const keyResp = await apiFetch<{ api_key: string }>('/auth/keys', {
        method: 'POST',
        body: { name: 'web' },
      })
      setKey(keyResp.api_key)
      await navigate({ to: '/portal' })
    } catch (err) {
      setError(describeError(err))
    } finally {
      setBusy(false)
    }
  }

  const submitPassword = async () => {
    setBusy(true)
    setError(null)
    try {
      await apiFetch('/auth/login', {
        method: 'POST',
        body: {
          email: form.email.trim(),
          password: form.password,
          totp_code: totpNeeded && form.totp ? form.totp : undefined,
        },
      })
      // 会话已建：兑 key 保持门户 key 单轨
      const keyResp = await apiFetch<{ api_key: string }>('/auth/keys', {
        method: 'POST',
        body: { name: 'web' },
      })
      setKey(keyResp.api_key)
      await navigate({ to: '/portal' })
    } catch (err) {
      if (err instanceof ApiError && err.code === 'totp_required') {
        setTotpNeeded(true)
      }
      setError(describeError(err))
    } finally {
      setBusy(false)
    }
  }

  const heading = {
    password: [t('auth:welcomeBack'), t('auth:signInSubtitle')],
    key: [t('auth:welcomeBack'), t('auth:keySubtitle')],
    register: [t('auth:createAccount'), t('auth:registerSubtitle')],
  }[tab]

  const oauth = providers.data?.providers ?? []

  return (
    <AuthLayout
      title={heading[0]}
      subtitle={heading[1]}
      footer={
        <Link to="/pricing" className="underline decoration-dotted underline-offset-4 hover:text-foreground">
          {t('pricing:title')}
        </Link>
      }
    >
      <div className="flex flex-col gap-5 rounded-xl border border-border bg-card p-6 shadow-card">
        <Segmented
          className="w-full [&>button]:flex-1"
          ariaLabel={t('common:login')}
          value={tab}
          onChange={switchTab}
          options={[
            { value: 'password', label: t('auth:tabPassword'), icon: Mail },
            { value: 'key', label: t('auth:tabKey'), icon: KeyRound },
            ...(regMode === 'closed'
              ? []
              : [{ value: 'register' as const, label: t('auth:tabRegister'), icon: UserPlus }]),
          ]}
        />

        {tab === 'register' && regMode === 'closed' && (
          <Alert tone="warning">{t('auth:registrationClosed')}</Alert>
        )}

        {tab === 'register' && regMode !== 'closed' && (
          <form
            className="flex flex-col gap-4"
            onSubmit={(e) => {
              e.preventDefault()
              void submitRegister()
            }}
          >
            <Field label={t('auth:email')} htmlFor="reg-email">
              <Input
                id="reg-email"
                type="email"
                autoComplete="email"
                value={regForm.email}
                onChange={(e) => setRegForm((f) => ({ ...f, email: e.target.value }))}
              />
            </Field>
            <Field label={t('auth:username')} htmlFor="reg-username">
              <Input
                id="reg-username"
                autoComplete="username"
                value={regForm.username}
                onChange={(e) => setRegForm((f) => ({ ...f, username: e.target.value }))}
              />
            </Field>
            <Field label={t('auth:passwordMin')} htmlFor="reg-password">
              <PasswordInput
                id="reg-password"
                autoComplete="new-password"
                value={regForm.password}
                onChange={(e) => setRegForm((f) => ({ ...f, password: e.target.value }))}
              />
            </Field>
            {regMode === 'invite_only' && !affFromUrl && (
              <Field label={t('auth:inviteCode')} htmlFor="reg-aff" hint={t('auth:inviteRequiredHint')}>
                <Input
                  id="reg-aff"
                  autoComplete="off"
                  value={affInput}
                  onChange={(e) => setAffInput(e.target.value)}
                />
              </Field>
            )}
            {(regPolicy.data?.allowed_domains.length ?? 0) > 0 && (
              <p className="text-xs text-muted-foreground">
                {t('auth:allowedDomains', { domains: regPolicy.data?.allowed_domains.join(', ') })}
              </p>
            )}
            {affFromUrl && (
              <Alert tone="success">{t('auth:affApplied', { code: affFromUrl })}</Alert>
            )}
            {giftMicro > 0 && (
              <Alert tone="success">
                {t('auth:registerGift', { amount: formatMoney(giftMicro, i18n.language) })}
              </Alert>
            )}
            {error && <Alert tone="destructive">{error}</Alert>}
            <Button
              type="submit"
              size="lg"
              loading={busy}
              disabled={
                !regForm.email.trim() ||
                !regForm.username.trim() ||
                regForm.password.length < 8 ||
                (regMode === 'invite_only' && !affCode)
              }
            >
              {t('auth:tabRegister')}
              <ArrowRight className="h-4 w-4" />
            </Button>
          </form>
        )}

        {tab === 'key' && (
          <form
            className="flex flex-col gap-4"
            onSubmit={(e) => {
              e.preventDefault()
              void submitKey()
            }}
          >
            <Field label={t('common:apiKey')} htmlFor="key">
              <PasswordInput
                id="key"
                autoComplete="off"
                value={key}
                placeholder={t('common:apiKeyPlaceholder')}
                className="font-mono"
                onChange={(e) => setKeyInput(e.target.value)}
              />
            </Field>
            {error && <Alert tone="destructive">{error}</Alert>}
            <Button type="submit" size="lg" loading={busy} disabled={!key.trim()}>
              {t('common:login')}
              <ArrowRight className="h-4 w-4" />
            </Button>
          </form>
        )}

        {tab === 'password' && (
          <form
            className="flex flex-col gap-4"
            onSubmit={(e) => {
              e.preventDefault()
              void submitPassword()
            }}
          >
            <Field label={t('auth:email')} htmlFor="email">
              <Input
                id="email"
                type="email"
                autoComplete="email"
                value={form.email}
                onChange={(e) => setForm((f) => ({ ...f, email: e.target.value }))}
              />
            </Field>
            <Field label={t('auth:password')} htmlFor="password">
              <PasswordInput
                id="password"
                autoComplete="current-password"
                value={form.password}
                onChange={(e) => setForm((f) => ({ ...f, password: e.target.value }))}
              />
            </Field>
            {totpNeeded && (
              <Field label={t('auth:totpCode')} htmlFor="totp">
                <Input
                  id="totp"
                  inputMode="numeric"
                  autoComplete="one-time-code"
                  autoFocus
                  className="font-mono tracking-[0.3em]"
                  value={form.totp}
                  onChange={(e) => setForm((f) => ({ ...f, totp: e.target.value }))}
                />
              </Field>
            )}
            {error && <Alert tone={totpNeeded && !form.totp ? 'info' : 'destructive'}>{error}</Alert>}
            <Button
              type="submit"
              size="lg"
              loading={busy || oauthPending}
              disabled={!form.email.trim() || !form.password}
            >
              {t('common:login')}
              <ArrowRight className="h-4 w-4" />
            </Button>
          </form>
        )}

        {oauth.length > 0 && tab !== 'key' && (
          <div className="flex flex-col gap-3">
            <div className="flex items-center gap-3 text-xs text-muted-foreground">
              <span className="h-px flex-1 bg-border" />
              {t('auth:orContinueWith')}
              <span className="h-px flex-1 bg-border" />
            </div>
            <div className="grid gap-2 sm:grid-cols-2">
              {oauth.map((p) => (
                <Button
                  key={p}
                  variant="outline"
                  onClick={() => {
                    window.location.href = `/auth/oauth/${p}`
                  }}
                >
                  {t('auth:oauthWith', { provider: p })}
                </Button>
              ))}
            </div>
          </div>
        )}

        <p className="text-center text-xs text-muted-foreground">
          {tab === 'register' ? (
            <button type="button" className="underline decoration-dotted underline-offset-4 hover:text-foreground" onClick={() => switchTab('password')}>
              {t('auth:switchToLogin')}
            </button>
          ) : (
            <button type="button" className="underline decoration-dotted underline-offset-4 hover:text-foreground" onClick={() => switchTab('register')}>
              {t('auth:switchToRegister')}
            </button>
          )}
        </p>
      </div>
    </AuthLayout>
  )
}
