import { useMutation } from '@tanstack/react-query'
import { ExternalLink, LogIn } from 'lucide-react'
import { useEffect, useId, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { Input, Label } from '@/components/ui/input'
import { toast } from '@/components/ui/toast'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { cn } from '@/lib/utils'
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
  /// 省略 = 后端缺省（all 范围进 default 池，own 范围建成孤儿）
  pools?: PoolMember[]
  cost_milli?: number
  /// 出口绑定（§11.41）：换码就从这个出口出去，固定分配组当场选定代理。
  egress?: import('@/features/proxies/types').EgressBinding
}
interface OAuthLoginProps {
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

/// 订阅 OAuth 登录（IMPLEMENTATION §11.38）的状态：两步——开授权页、贴回 code。
/// 不监听本地回调：网关多半在服务器上、浏览器在站长电脑上，贴回 code 对所有形态都成立。
///
/// 状态与界面分开：新建渠道的引导把「换取凭证并创建」放在抽屉底部的主按钮上（最后一步），
/// 编辑时的重新授权则是卡片里自己的按钮，两处共用这一份状态。
export function useOAuthLogin({ provider, name, models, channelId, channelKeyId, onDone, settings, valid = true, maxConcurrency, creationOptions }: OAuthLoginProps) {
  const { t } = useTranslation()
  const account = useAccountCapabilities(provider)
  const authorization = account.capabilities?.authorization
  const [started, setStarted] = useState<StartResp | null>(null)
  const [code, setCode] = useState('')
  // 换了协议（引导里退回第一步改选）：之前开的授权页属于另一家，state 与 code 都作废
  useEffect(() => {
    setStarted(null)
    setCode('')
  }, [provider])

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

  const canAuthorize = valid && Boolean(authorization)
  const ready = canAuthorize && (channelId !== undefined ? channelKeyId !== undefined : name.trim() !== '' && models.length > 0)
  return {
    account,
    codeState: authorization?.code_format === 'code_state',
    /// 能不能开授权页：编辑时还得先选定要换的那把 key
    canStart: canAuthorize && (channelId === undefined || channelKeyId !== undefined),
    ready,
    started,
    code,
    setCode,
    start,
    exchange,
    canExchange: started !== null && code.trim() !== '' && ready && !exchange.isPending,
  }
}

export type OAuthLogin = ReturnType<typeof useOAuthLogin>

function StepRow({ index, title, muted = false, children }: { index: number; title: React.ReactNode; muted?: boolean; children: React.ReactNode }) {
  return (
    <li className="flex min-w-0 gap-3">
      <span
        aria-hidden
        className={cn(
          'mt-0.5 flex h-5 w-5 shrink-0 items-center justify-center rounded-full text-[11px] font-medium tabular-nums',
          muted ? 'border border-border text-muted-foreground' : 'bg-primary/12 text-primary',
        )}
      >
        {index}
      </span>
      <div className="flex min-w-0 flex-1 flex-col gap-2">
        <div className={cn('text-sm font-medium', muted && 'text-muted-foreground')}>{title}</div>
        {children}
      </div>
    </li>
  )
}

/// 登录的两步：① 开授权页登录 ② 把 code 贴回来。第二步在开过授权页之前是灰的——
/// 整个流程一眼看得到，但顺序不会走错。
export function OAuthLoginSteps({ login, codeId, action }: { login: OAuthLogin; codeId: string; action?: React.ReactNode }) {
  const { t } = useTranslation()
  const { account, started, code, setCode, start, codeState } = login
  return (
    <div data-slot="oauth-login" className="flex min-w-0 flex-col gap-4">
      {account.isError && <div className="flex items-center gap-2"><p role="alert" className="text-xs text-destructive">{describeError(account.error)}</p><Button size="sm" variant="outline" onClick={() => void account.refetch()}>{t('common:retry')}</Button></div>}
      <ol className="flex min-w-0 flex-col gap-5">
        <StepRow index={1} title={t('admin:oauthStepOpen')}>
          <p className="text-xs leading-5 text-muted-foreground">{t('admin:oauthAuthorizationHint')}</p>
          <div className="flex flex-wrap items-center gap-x-3 gap-y-2">
            <Button type="button" variant={started ? 'outline' : 'default'} loading={start.isPending} disabled={!login.canStart} onClick={() => start.mutate()}>
              <LogIn className="h-4 w-4" />
              {started ? t('admin:oauthReopen') : t('admin:oauthStart')}
            </Button>
            {started && (
              <a href={started.authorize_url} target="_blank" rel="noreferrer noopener" className="inline-flex items-center gap-1 text-xs text-primary hover:underline">
                <ExternalLink className="h-3.5 w-3.5" />
                {t('admin:oauthOpenLink')}
              </a>
            )}
          </div>
        </StepRow>
        <StepRow index={2} muted={!started} title={<Label htmlFor={codeId} className={cn('text-sm', !started && 'text-muted-foreground')}>{t('admin:oauthPasteCode')}</Label>}>
          <Input
            id={codeId}
            className="font-mono text-xs"
            value={code}
            disabled={!started}
            placeholder={started ? (codeState ? 'code#state' : `${started.redirect_uri}?code=…`) : t('admin:oauthPasteAfterOpen')}
            onChange={(e) => setCode(e.target.value)}
          />
          {started && (
            <p className="text-xs leading-5 text-muted-foreground">
              {t(codeState ? 'admin:oauthPasteCodeState' : 'admin:oauthPasteCallback', { uri: started.redirect_uri })}
            </p>
          )}
          {action}
        </StepRow>
      </ol>
      <p className="rounded-lg border border-warning/25 bg-warning/8 px-3 py-2 text-xs leading-5 text-warning">{t('admin:oauthExperimental')}</p>
    </div>
  )
}

/// 编辑渠道时的重新授权：同样两步，换码按钮就在卡片里（它只更新这一把 key，不是整张表单的保存）。
export function OAuthLoginCard(props: OAuthLoginProps) {
  const { t } = useTranslation()
  const login = useOAuthLogin(props)
  const uniqueId = useId()
  const codeId = props.channelKeyId === undefined ? 'oauth-code' : uniqueId
  return (
    <OAuthLoginSteps
      login={login}
      codeId={codeId}
      action={login.started && (
        <Button type="button" className="self-start" disabled={!login.canExchange} loading={login.exchange.isPending} onClick={() => login.exchange.mutate()}>
          {t('admin:oauthReauthorizeSave')}
        </Button>
      )}
    />
  )
}
