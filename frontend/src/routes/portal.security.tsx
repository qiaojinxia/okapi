import { useMutation } from '@tanstack/react-query'
import { createFileRoute } from '@tanstack/react-router'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Input, Label } from '@/components/ui/input'
import { ApiError, apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'

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
/// otpauth 链接以只读输入框呈现供手动录入，不渲染二维码：二维码需引入前端库，
/// 而 §1 选型已冻结，为一个绑定流程加依赖不划算。
function SecurityPage() {
  const { t } = useTranslation()
  const [enrolled, setEnrolled] = useState<EnrollResp | null>(null)
  const [code, setCode] = useState('')
  const [msg, setMsg] = useState<string | null>(null)
  const [needSession, setNeedSession] = useState(false)

  const fail = (err: unknown) => {
    if (err instanceof ApiError && err.status === 401) {
      setNeedSession(true)
      return
    }
    setMsg(describeError(err))
  }

  const enroll = useMutation({
    mutationFn: () => apiFetch<EnrollResp>('/auth/totp/enroll', { method: 'POST', body: {} }),
    onSuccess: (r) => {
      setEnrolled(r)
      setMsg(null)
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
      setMsg(t('security:enabled'))
      setEnrolled(null)
      setCode('')
    },
    onError: fail,
  })

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('security:title')}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        <p className="text-xs text-muted-foreground">{t('security:hint')}</p>

        {needSession ? (
          <p className="text-sm text-muted-foreground">{t('security:sessionRequired')}</p>
        ) : enrolled === null ? (
          <div className="flex items-center gap-3">
            <Button disabled={enroll.isPending} onClick={() => enroll.mutate()}>
              {t('security:start')}
            </Button>
            {msg !== null && <span className="text-xs text-muted-foreground">{msg}</span>}
          </div>
        ) : (
          <div className="flex flex-col gap-3">
            <div className="flex flex-col gap-1.5">
              <Label htmlFor="otpauth">{t('security:otpauthUrl')}</Label>
              <Input
                id="otpauth"
                readOnly
                className="font-mono text-xs"
                value={enrolled.otpauth_url}
              />
              <span className="text-xs text-muted-foreground">{t('security:otpauthHint')}</span>
            </div>
            <div className="flex flex-wrap items-end gap-3">
              <div className="flex flex-col gap-1.5">
                <Label htmlFor="code">{t('auth:totpCode')}</Label>
                <Input
                  id="code"
                  className="w-32"
                  inputMode="numeric"
                  value={code}
                  onChange={(e) => setCode(e.target.value)}
                />
              </div>
              <Button
                disabled={confirm.isPending || code.trim().length < 6}
                onClick={() => confirm.mutate()}
              >
                {t('security:confirm')}
              </Button>
              <Button variant="ghost" onClick={() => setEnrolled(null)}>
                {t('common:cancel')}
              </Button>
              {msg !== null && <span className="text-xs text-muted-foreground">{msg}</span>}
            </div>
          </div>
        )}
      </CardContent>
    </Card>
  )
}
