import { useNavigate } from '@tanstack/react-router'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Input, Label } from '@/components/ui/input'
import { apiFetch, setKey } from '@/lib/api'
import { describeError } from '@/lib/i18n'

/// 首启向导：创建超管 → 一次性展示 key → 直接进门户。
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

  return (
    <div className="flex min-h-screen items-center justify-center p-4">
      <Card className="w-full max-w-md">
        <CardHeader>
          <div className="text-xl font-bold text-primary">{t('setup:title')}</div>
          <CardTitle>{t('setup:hint')}</CardTitle>
        </CardHeader>
        <CardContent className="flex flex-col gap-3">
          {apiKey ? (
            <>
              <p className="text-xs text-destructive">{t('setup:keyNote')}</p>
              <code className="break-all rounded-md bg-muted p-3 font-mono text-xs">{apiKey}</code>
              <Button onClick={() => void navigate({ to: '/admin' })}>{t('setup:enter')}</Button>
            </>
          ) : (
            <>
              <div className="flex flex-col gap-1.5">
                <Label htmlFor="username">{t('setup:username')}</Label>
                <Input
                  id="username"
                  value={username}
                  onChange={(e) => setUsername(e.target.value)}
                />
              </div>
              {error && <p className="text-xs text-destructive">{error}</p>}
              <Button disabled={busy || !username.trim()} onClick={() => void submit()}>
                {busy ? t('common:loading') : t('setup:submit')}
              </Button>
            </>
          )}
        </CardContent>
      </Card>
    </div>
  )
}
