import { useNavigate } from '@tanstack/react-router'
import { ArrowRight, KeyRound, ShieldCheck } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Alert } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { CopyButton } from '@/components/ui/copy-button'
import { Field } from '@/components/ui/field'
import { Input } from '@/components/ui/input'
import { AuthLayout } from '@/features/auth/AuthLayout'
import { apiFetch, setKey } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { cn } from '@/lib/utils'

/// 首启向导：创建超管 → 一次性展示 key → 直接进控制台。
/// 两步各占一屏，顶部步骤条告诉用户"还有几步"；key 只显示一次，故给复制按钮。
export function SetupWizard() {
  const { t } = useTranslation()
  const navigate = useNavigate()
  const [username, setUsername] = useState('')
  const [apiKey, setApiKey] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  const submit = async () => {
    setBusy(true)
    setError(null)
    try {
      const resp = await apiFetch<{ api_key: string }>('/api/setup', {
        method: 'POST',
        body: { username: username.trim() },
      })
      setApiKey(resp.api_key)
      setKey(resp.api_key)
    } catch (err) {
      setError(describeError(err))
    } finally {
      setBusy(false)
    }
  }

  const step = apiKey ? 2 : 1
  const steps = [
    { n: 1, icon: ShieldCheck, label: t('setup:step1') },
    { n: 2, icon: KeyRound, label: t('setup:step2') },
  ]

  return (
    <AuthLayout title={t('setup:title')} subtitle={t('setup:hint')}>
      <div className="flex flex-col gap-5 rounded-xl border border-border bg-card p-6 shadow-card">
        <ol className="flex items-center gap-2 text-xs">
          {steps.map((s, i) => {
            const reached = step >= s.n
            return (
              <li key={s.n} className="flex flex-1 items-center gap-2">
                <span
                  className={cn(
                    'flex h-6 w-6 shrink-0 items-center justify-center rounded-full',
                    reached ? 'bg-primary text-primary-foreground' : 'bg-muted text-muted-foreground',
                  )}
                >
                  <s.icon className="h-3.5 w-3.5" />
                </span>
                <span className={step === s.n ? 'font-medium' : 'text-muted-foreground'}>
                  {s.label}
                </span>
                {i < steps.length - 1 && <span className="mx-1 h-px flex-1 bg-border" aria-hidden />}
              </li>
            )
          })}
        </ol>

        {apiKey ? (
          <div className="flex flex-col gap-4">
            <Alert tone="warning" title={t('setup:keyNote')}>
              {t('portal:keyMintedHint')}
            </Alert>
            <div className="flex items-center gap-2 rounded-lg border border-border bg-muted/40 p-3">
              <code className="min-w-0 flex-1 font-mono text-xs break-all">{apiKey}</code>
              <CopyButton value={apiKey} />
            </div>
            <Button size="lg" onClick={() => void navigate({ to: '/admin' })}>
              {t('setup:enter')}
              <ArrowRight className="h-4 w-4" />
            </Button>
          </div>
        ) : (
          <form
            className="flex flex-col gap-4"
            onSubmit={(e) => {
              e.preventDefault()
              void submit()
            }}
          >
            <Field label={t('setup:username')} htmlFor="username">
              <Input
                id="username"
                autoFocus
                autoComplete="username"
                value={username}
                onChange={(e) => setUsername(e.target.value)}
              />
            </Field>
            {error && <Alert tone="destructive">{error}</Alert>}
            <Button type="submit" size="lg" loading={busy} disabled={!username.trim()}>
              {t('setup:submit')}
              <ArrowRight className="h-4 w-4" />
            </Button>
          </form>
        )}
      </div>
    </AuthLayout>
  )
}
