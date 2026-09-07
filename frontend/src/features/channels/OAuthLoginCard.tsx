import { useMutation } from '@tanstack/react-query'
import { ExternalLink, LogIn } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { Input, Label } from '@/components/ui/input'
import { toast } from '@/components/ui/toast'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'

interface StartResp {
  authorize_url: string
  state: string
  redirect_uri: string
}

interface ExchangeResp {
  channel_id: number
  channel_key_id: number
  expires_at: number
  account_id: string | null
}

interface OAuthLoginCardProps {
  provider: 'anthropic_max' | 'codex'
  /// 新建：渠道名 + 模型（后端据此建渠道）；追加 key：给 channelId。
  name: string
  models: string[]
  channelId?: number
  onDone: (resp: ExchangeResp) => void
}

/// 订阅 OAuth 登录（IMPLEMENTATION §11.38）：两步——开授权页、贴回 code。
/// 不监听本地回调：网关多半在服务器上、浏览器在站长电脑上，贴回 code 对所有形态都成立。
export function OAuthLoginCard({ provider, name, models, channelId, onDone }: OAuthLoginCardProps) {
  const { t } = useTranslation()
  const [started, setStarted] = useState<StartResp | null>(null)
  const [code, setCode] = useState('')

  const start = useMutation({
    mutationFn: () =>
      apiFetch<StartResp>('/admin/channels/oauth/start', { method: 'POST', body: { provider } }),
    onSuccess: (r) => {
      setStarted(r)
      window.open(r.authorize_url, '_blank', 'noopener,noreferrer')
    },
    onError: (err) => toast.error(describeError(err)),
  })

  const exchange = useMutation({
    mutationFn: () =>
      apiFetch<ExchangeResp>('/admin/channels/oauth/exchange', {
        method: 'POST',
        body: {
          state: started?.state,
          code: code.trim(),
          ...(channelId === undefined ? { name: name.trim(), models } : { channel_id: channelId }),
        },
      }),
    onSuccess: (r) => {
      toast.success(t('admin:oauthLoginDone'))
      setStarted(null)
      setCode('')
      onDone(r)
    },
    onError: (err) => toast.error(describeError(err)),
  })

  const readyToCreate = channelId !== undefined || (name.trim() !== '' && models.length > 0)

  return (
    <div className="flex flex-col gap-3 rounded-lg border border-border p-4">
      <p className="text-xs leading-5 text-muted-foreground">
        {t(provider === 'anthropic_max' ? 'admin:oauthHintAnthropic' : 'admin:oauthHintCodex')}
      </p>
      <p className="text-xs leading-5 text-warning">{t('admin:oauthExperimental')}</p>
      <div className="flex flex-wrap items-center gap-2">
        <Button
          type="button"
          variant="outline"
          loading={start.isPending}
          onClick={() => start.mutate()}
        >
          <LogIn className="h-4 w-4" />
          {started ? t('admin:oauthReopen') : t('admin:oauthStart')}
        </Button>
        {started && (
          <a
            href={started.authorize_url}
            target="_blank"
            rel="noreferrer noopener"
            className="inline-flex items-center gap-1 text-xs text-primary"
          >
            <ExternalLink className="h-3.5 w-3.5" />
            {t('admin:oauthOpenLink')}
          </a>
        )}
      </div>
      {started && (
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="oauth-code">{t('admin:oauthPasteCode')}</Label>
          <Input
            id="oauth-code"
            className="font-mono text-xs"
            value={code}
            placeholder={provider === 'anthropic_max' ? 'code#state' : 'http://localhost:1455/auth/callback?code=…'}
            onChange={(e) => setCode(e.target.value)}
          />
          <p className="text-xs text-muted-foreground">
            {t(provider === 'anthropic_max' ? 'admin:oauthPasteHintAnthropic' : 'admin:oauthPasteHintCodex')}
          </p>
          <Button
            type="button"
            className="self-start"
            disabled={code.trim() === '' || !readyToCreate || exchange.isPending}
            loading={exchange.isPending}
            onClick={() => exchange.mutate()}
          >
            {channelId === undefined ? t('admin:oauthCreateChannel') : t('admin:oauthAddKey')}
          </Button>
          {!readyToCreate && (
            <p className="text-xs text-destructive">{t('admin:oauthNeedNameModels')}</p>
          )}
        </div>
      )}
    </div>
  )
}
