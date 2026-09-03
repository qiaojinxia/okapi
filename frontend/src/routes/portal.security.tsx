import { useMutation } from '@tanstack/react-query'
import { createFileRoute } from '@tanstack/react-router'
import { KeyRound, ShieldCheck, Smartphone } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Alert } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { CopyButton } from '@/components/ui/copy-button'
import { Field } from '@/components/ui/field'
import { Input } from '@/components/ui/input'
import { PageHeader } from '@/components/ui/page'
import { RecentLoginsCard } from '@/features/security/RecentLoginsCard'
import { toast } from '@/components/ui/toast'
import { ApiError, apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { cn } from '@/lib/utils'

export const Route = createFileRoute('/portal/security')({
  component: SecurityPage,
})

interface EnrollResp {
  otpauth_url: string
  /// 服务端密文回执：密钥仍在服务端（信封加密），客户端只是转交回去 confirm。
  pending: string
}

/// 两步验证自助绑定。
///
/// 与 Team 层同一约束：TOTP 端点走 web session 鉴权，用 API Key 方式登录的浏览器
/// 没有 session cookie 会 401 —— 故此处把 401 降级为"请改用邮箱密码登录"提示，
/// 而不是显示一个点了没反应的按钮。
///
/// otpauth 链接以文本 + 复制按钮呈现供手动录入，不渲染二维码：二维码需引入前端库，
/// 而 §1 选型已冻结，为一个绑定流程加依赖不划算。
function SecurityPage() {
  const { t } = useTranslation()
  const [enrolled, setEnrolled] = useState<EnrollResp | null>(null)
  const [code, setCode] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [done, setDone] = useState(false)
  const [needSession, setNeedSession] = useState(false)

  const fail = (err: unknown) => {
    if (err instanceof ApiError && err.status === 401) {
      setNeedSession(true)
      return
    }
    setError(describeError(err))
  }

  const enroll = useMutation({
    mutationFn: () => apiFetch<EnrollResp>('/auth/totp/enroll', { method: 'POST', body: {} }),
    onSuccess: (r) => {
      setEnrolled(r)
      setError(null)
    },
    onError: fail,
  })

  const confirm = useMutation({
    mutationFn: () =>
      apiFetch<{ enabled: boolean }>('/auth/totp/confirm', {
        method: 'POST',
        body: { pending: enrolled?.pending, code: code.trim() },
      }),
    onSuccess: () => {
      toast.success(t('security:enabled'))
      setDone(true)
      setEnrolled(null)
      setCode('')
      setError(null)
    },
    onError: fail,
  })

  const step = done ? 3 : enrolled ? 2 : 1

  return (
    <div className="flex flex-col gap-4">
      <PageHeader title={t('security:nav')} description={t('security:desc')} icon={ShieldCheck} />

      <div className="grid gap-4 lg:grid-cols-[minmax(0,3fr)_minmax(0,2fr)]">
        <Card>
          <CardHeader>
            <CardTitle>{t('security:title')}</CardTitle>
            <CardDescription>{t('security:hint')}</CardDescription>
          </CardHeader>
          <CardContent className="flex flex-col gap-5 pt-3">
            {needSession ? (
              <Alert tone="warning">{t('security:sessionRequired')}</Alert>
            ) : done ? (
              <Alert tone="success" title={t('security:enabled')}>
                {t('security:enabledHint')}
              </Alert>
            ) : (
              <>
                <Step n={1} active={step === 1} done={step > 1} title={t('security:step1Title')} hint={t('security:step1Hint')}>
                  {step === 1 && (
                    <Button loading={enroll.isPending} onClick={() => enroll.mutate()}>
                      <Smartphone className="h-4 w-4" />
                      {t('security:start')}
                    </Button>
                  )}
                </Step>
                <Step n={2} active={step === 2} done={false} title={t('security:step2Title')} hint={t('security:otpauthHint')}>
                  {enrolled !== null && (
                    <div className="flex flex-col gap-4">
                      <div className="flex items-center gap-2 rounded-lg border border-border bg-muted/40 p-3">
                        <code className="min-w-0 flex-1 font-mono text-xs break-all">
                          {enrolled.otpauth_url}
                        </code>
                        <CopyButton value={enrolled.otpauth_url} label={t('security:copyUrl')} />
                      </div>
                      <form
                        className="flex flex-wrap items-end gap-3"
                        onSubmit={(e) => {
                          e.preventDefault()
                          if (code.trim().length >= 6) confirm.mutate()
                        }}
                      >
                        <Field label={t('auth:totpCode')} htmlFor="code" error={error}>
                          <Input
                            id="code"
                            className="w-40 font-mono text-base tracking-[0.3em]"
                            inputMode="numeric"
                            autoComplete="one-time-code"
                            autoFocus
                            maxLength={8}
                            value={code}
                            onChange={(e) => {
                              setCode(e.target.value)
                              setError(null)
                            }}
                          />
                        </Field>
                        <Button type="submit" loading={confirm.isPending} disabled={code.trim().length < 6}>
                          <KeyRound className="h-4 w-4" />
                          {t('security:confirm')}
                        </Button>
                        <Button variant="ghost" onClick={() => setEnrolled(null)}>
                          {t('common:cancel')}
                        </Button>
                      </form>
                    </div>
                  )}
                </Step>
              </>
            )}
            {error !== null && enrolled === null && !needSession && (
              <Alert tone="destructive">{error}</Alert>
            )}
          </CardContent>
        </Card>

        <div className="flex flex-col gap-4">
          <Card className="bg-muted/30">
            <CardHeader>
              <CardTitle>{t('security:whyTitle')}</CardTitle>
            </CardHeader>
            <CardContent className="flex flex-col gap-3 pt-2 text-sm text-muted-foreground">
              <p>{t('security:why1')}</p>
              <p>{t('security:why2')}</p>
            </CardContent>
          </Card>
          {/* 最近登录紧挨两步验证：看到不是自己的记录，动作就在左边那张卡里 */}
          <RecentLoginsCard />
        </div>
      </div>
    </div>
  )
}

function Step({
  n,
  active,
  done,
  title,
  hint,
  children,
}: {
  n: number
  active: boolean
  done: boolean
  title: string
  hint: string
  children?: React.ReactNode
}) {
  return (
    <div className={cn('flex gap-4', !active && !done && 'opacity-60')}>
      <span
        className={cn(
          'flex h-7 w-7 shrink-0 items-center justify-center rounded-full text-xs font-semibold',
          active || done ? 'bg-primary text-primary-foreground' : 'bg-muted text-muted-foreground',
        )}
      >
        {done ? '✓' : n}
      </span>
      <div className="flex min-w-0 flex-1 flex-col items-start gap-2 [&>form]:w-full">
        <div className="flex flex-col gap-0.5 pt-1">
          <span className="text-sm font-medium">{title}</span>
          <span className="text-xs leading-5 text-muted-foreground">{hint}</span>
        </div>
        {children}
      </div>
    </div>
  )
}
