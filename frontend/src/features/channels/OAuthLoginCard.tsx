import { useMutation } from '@tanstack/react-query'
import { ExternalLink, LogIn } from 'lucide-react'
import { useId, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { Input, Label } from '@/components/ui/input'
import { toast } from '@/components/ui/toast'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { useAccountCapabilities } from './account-controls/api'
import type { PoolMember } from '@/features/pools/types'

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

interface CreationOptions {
  api_base: string
  priority: number
  pools: PoolMember[]
  cost_milli?: number
  data_retention: string
  /// 出口绑定（§11.41）：换码就从这个出口出去，固定分配组当场选定代理。
  egress?: import('@/features/proxies/types').EgressBinding
}
interface OAuthLoginCardProps {
  creationOptions?: CreationOptions
  settings?: import('./types').ChannelSettings
  valid?: boolean
  maxConcurrency?: number
  provider: string
  /// One credential per channel; editing binds reauthorization to the existing key.
  name: string
  models: string[]
  channelId?: number
  channelKeyId?: number
  onDone: (resp: ExchangeResp) => void
}

/// 订阅 OAuth 登录（IMPLEMENTATION §11.38）：两步——开授权页、贴回 code。
/// 不监听本地回调：网关多半在服务器上、浏览器在站长电脑上，贴回 code 对所有形态都成立。
export function OAuthLoginCard({ provider, name, models, channelId, channelKeyId, onDone, settings, valid = true, maxConcurrency, creationOptions }: OAuthLoginCardProps) {
  const { t } = useTranslation()
  const account = useAccountCapabilities(provider)
  const authorization = account.capabilities?.authorization
  const codeState = authorization?.code_format === 'code_state'
  const canAuthorize = valid && Boolean(authorization)
  const uniqueId = useId()
  const codeId = channelKeyId === undefined ? 'oauth-code' : uniqueId
  const [started, setStarted] = useState<StartResp | null>(null)
  const [code, setCode] = useState('')

  const start = useMutation({
    mutationFn: () =>
      apiFetch<StartResp>('/admin/channels/oauth/start', { method: 'POST', body: {
        provider,
        ...(channelKeyId === undefined ? {} : { channel_id: channelId, channel_key_id: channelKeyId }),
      } }),
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
          ...(channelKeyId === undefined ? {} : { channel_key_id: channelKeyId }),
          ...(channelId === undefined ? creationOptions : {}),
          ...(channelId === undefined && settings ? { settings } : {}),
          ...(channelId === undefined && maxConcurrency !== undefined ? { max_concurrency: maxConcurrency } : {}),
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

  const readyToCreate = canAuthorize && (channelId !== undefined ? channelKeyId !== undefined : name.trim() !== '' && models.length > 0)

  return (
    <div className="flex flex-col gap-3 rounded-lg border border-border p-4">
      <p className="text-xs leading-5 text-muted-foreground">
        {t('admin:oauthAuthorizationHint')}
      </p>
      {account.isError && <div className="flex items-center gap-2"><p role="alert" className="text-xs text-destructive">{describeError(account.error)}</p><Button size="sm" variant="outline" onClick={() => void account.refetch()}>{t('common:retry')}</Button></div>}
      <p className="text-xs leading-5 text-warning">{t('admin:oauthExperimental')}</p>
      <div className="flex flex-wrap items-center gap-2">
        <Button
          type="button"
          variant="outline"
          loading={start.isPending}
          disabled={!canAuthorize || (channelId !== undefined && channelKeyId === undefined)}
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
          <Label htmlFor={codeId}>{t('admin:oauthPasteCode')}</Label>
          <Input
            id={codeId}
            className="font-mono text-xs"
            value={code}
            placeholder={codeState ? 'code#state' : `${started.redirect_uri}?code=…`}
            onChange={(e) => setCode(e.target.value)}
          />
          <p className="text-xs text-muted-foreground">
            {t(codeState ? 'admin:oauthPasteCodeState' : 'admin:oauthPasteCallback', { uri: started.redirect_uri })}
          </p>
          <Button
            type="button"
            className="self-start"
          disabled={code.trim() === '' || !readyToCreate || exchange.isPending}
            loading={exchange.isPending}
            onClick={() => exchange.mutate()}
          >
            {channelId !== undefined ? t('admin:oauthReauthorizeSave') : t('admin:oauthCreateChannel')}
          </Button>
          {!readyToCreate && (
            <p className="text-xs text-destructive">{t('admin:oauthNeedNameModels')}</p>
          )}
        </div>
      )}
    </div>
  )
}
