import { Link, useNavigate } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { ApiError, apiFetch, setKey } from '@/lib/api'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Input, Label } from '@/components/ui/input'
import { describeError } from '@/lib/i18n'
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


export function LoginForm() {
  const { t } = useTranslation()
  const navigate = useNavigate()
  const [tab, setTab] = useState<LoginTab>('password')
  const [key, setKeyInput] = useState('')
  const [form, setForm] = useState({ email: '', password: '', totp: '' })
  const [regForm, setRegForm] = useState({ email: '', username: '', password: '' })
  const [totpNeeded, setTotpNeeded] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  // 邀请链接落地（/?aff=code）：注册请求隐式携带
  const affCode = new URLSearchParams(window.location.search).get('aff')

  const oauthPending = useOauthLanding(setError)
  const providers = useQuery({
    queryKey: qk.oauthProviders,
    queryFn: () => apiFetch<{ providers: string[] }>('/auth/oauth-providers'),
    retry: 0,
  })

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
        setError(describeError(err))
      } else {
        setError(describeError(err))
      }
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="flex min-h-screen items-center justify-center p-4">
      <Card className="w-full max-w-sm">
        <CardHeader>
          <div className="text-xl font-bold text-primary">{t('common:appName')}</div>
          <CardTitle>{t('portal:loginHint')}</CardTitle>
        </CardHeader>
        <CardContent className="flex flex-col gap-3">
          <div className="flex gap-1">
            <Button
              size="sm"
              variant={tab === 'password' ? 'default' : 'outline'}
              onClick={() => setTab('password')}
            >
              {t('auth:tabPassword')}
            </Button>
            <Button
              size="sm"
              variant={tab === 'key' ? 'default' : 'outline'}
              onClick={() => setTab('key')}
            >
              {t('auth:tabKey')}
            </Button>
            <Button
              size="sm"
              variant={tab === 'register' ? 'default' : 'outline'}
              onClick={() => setTab('register')}
            >
              {t('auth:tabRegister')}
            </Button>
          </div>

          {tab === 'register' && (
            <>
              <div className="flex flex-col gap-1.5">
                <Label htmlFor="reg-email">{t('auth:email')}</Label>
                <Input
                  id="reg-email"
                  type="email"
                  value={regForm.email}
                  onChange={(e) => setRegForm((f) => ({ ...f, email: e.target.value }))}
                />
              </div>
              <div className="flex flex-col gap-1.5">
                <Label htmlFor="reg-username">{t('auth:username')}</Label>
                <Input
                  id="reg-username"
                  value={regForm.username}
                  onChange={(e) => setRegForm((f) => ({ ...f, username: e.target.value }))}
                />
              </div>
              <div className="flex flex-col gap-1.5">
                <Label htmlFor="reg-password">{t('auth:passwordMin')}</Label>
                <Input
                  id="reg-password"
                  type="password"
                  value={regForm.password}
                  onChange={(e) => setRegForm((f) => ({ ...f, password: e.target.value }))}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter') void submitRegister()
                  }}
                />
              </div>
              {affCode && (
                <p className="text-xs text-muted-foreground">
                  {t('auth:affApplied', { code: affCode })}
                </p>
              )}
              <Button
                disabled={
                  busy ||
                  !regForm.email.trim() ||
                  !regForm.username.trim() ||
                  regForm.password.length < 8
                }
                onClick={() => void submitRegister()}
              >
                {busy ? t('common:loading') : t('auth:tabRegister')}
              </Button>
            </>
          )}

          {tab === 'key' && (
            <>
              <div className="flex flex-col gap-1.5">
                <Label htmlFor="key">{t('common:apiKey')}</Label>
                <Input
                  id="key"
                  type="password"
                  value={key}
                  placeholder={t('common:apiKeyPlaceholder')}
                  onChange={(e) => setKeyInput(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter') void submitKey()
                  }}
                />
              </div>
              <Button disabled={busy || !key.trim()} onClick={() => void submitKey()}>
                {busy ? t('common:loading') : t('common:login')}
              </Button>
            </>
          )}

          {tab === 'password' && (
            <>
              <div className="flex flex-col gap-1.5">
                <Label htmlFor="email">{t('auth:email')}</Label>
                <Input
                  id="email"
                  type="email"
                  value={form.email}
                  onChange={(e) => setForm((f) => ({ ...f, email: e.target.value }))}
                />
              </div>
              <div className="flex flex-col gap-1.5">
                <Label htmlFor="password">{t('auth:password')}</Label>
                <Input
                  id="password"
                  type="password"
                  value={form.password}
                  onChange={(e) => setForm((f) => ({ ...f, password: e.target.value }))}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter') void submitPassword()
                  }}
                />
              </div>
              {totpNeeded && (
                <div className="flex flex-col gap-1.5">
                  <Label htmlFor="totp">{t('auth:totpCode')}</Label>
                  <Input
                    id="totp"
                    inputMode="numeric"
                    value={form.totp}
                    onChange={(e) => setForm((f) => ({ ...f, totp: e.target.value }))}
                  />
                </div>
              )}
              <Button
                disabled={busy || !form.email.trim() || !form.password}
                onClick={() => void submitPassword()}
              >
                {busy || oauthPending ? t('common:loading') : t('common:login')}
              </Button>
            </>
          )}

          {(providers.data?.providers ?? []).length > 0 && (
            <div className="flex flex-col gap-1.5 border-t border-border pt-3">
              {(providers.data?.providers ?? []).map((p) => (
                <Button
                  key={p}
                  variant="outline"
                  size="sm"
                  onClick={() => {
                    window.location.href = `/auth/oauth/${p}`
                  }}
                >
                  {t('auth:oauthWith', { provider: p })}
                </Button>
              ))}
            </div>
          )}

          {error && <p className="text-xs text-destructive">{error}</p>}
          <Link
            to="/pricing"
            className="text-center text-xs text-muted-foreground hover:text-foreground"
          >
            {t('pricing:title')}
          </Link>
        </CardContent>
      </Card>
    </div>
  )
}
