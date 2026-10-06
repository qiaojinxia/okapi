import { useMutation } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { toast } from '@/components/ui/toast'
import { apiFetch } from '@/lib/api'
import { describeError } from '@/lib/i18n'
import { OAuthLoginCard } from './OAuthLoginCard'
import { useAccountCapabilities } from './account-controls/api'
import type { ChannelKeyRow } from './types'

interface Props {
  channelId: number
  provider: string
  row: ChannelKeyRow
  refreshEnabled: boolean
  /** Hide the inline reauthorization when the surrounding form already offers it. */
  reauthorize?: boolean
  onDone: () => void
}

export function OAuthKeyHealth({ channelId, provider, row, refreshEnabled, reauthorize = true, onDone }: Props) {
  const { t, i18n } = useTranslation()
  const { capabilities } = useAccountCapabilities(provider)
  const [reauthorizing, setReauthorizing] = useState(false)
  const refresh = useMutation({
    mutationFn: () => apiFetch(`/admin/channels/${channelId}/keys/${row.id}/oauth/refresh`, { method: 'POST' }),
    onSuccess: () => { toast.success(t('common:success')); onDone() },
    onError: (error) => { toast.error(describeError(error)); onDone() },
  })
  const date = (seconds: number) => new Intl.DateTimeFormat(i18n.language, {
    dateStyle: 'medium', timeStyle: 'short',
  }).format(new Date(seconds * 1000))
  const health = row.oauth_refresh
  return (
    <div className="flex w-full flex-col gap-2 border-t border-border pt-2 text-xs">
      {row.credential_expires_at !== undefined && (
        <span className="text-muted-foreground">{t('admin:oauthExpires', { at: date(row.credential_expires_at) })}</span>
      )}
      {row.oauth_refreshable === false && <span className="text-muted-foreground">{t('admin:oauthTokenOnly')}</span>}
      {row.status === 6 && <span className="text-destructive">{t('admin:oauthReauthRequired')}</span>}
      {health?.last_success_at && <span className="text-muted-foreground">{t('admin:oauthRefreshSuccess', { at: date(health.last_success_at) })}</span>}
      {health?.error_code && <span className="text-destructive">{t('admin:oauthRefreshFailed', { code: health.error_code })}</span>}
      {row.status !== 6 && health?.next_retry_at && <span className="text-muted-foreground">{t('admin:oauthRefreshRetry', { at: date(health.next_retry_at) })}</span>}
      <div className="flex gap-2">
        {capabilities?.refresh && refreshEnabled && row.status <= 3 && row.oauth_refreshable !== false && <Button size="sm" variant="outline" disabled={refresh.isPending} onClick={() => refresh.mutate()}>{t('admin:oauthRefreshNow')}</Button>}
        {reauthorize && capabilities?.authorization && <Button size="sm" variant="outline" onClick={() => setReauthorizing(!reauthorizing)}>{reauthorizing ? t('common:cancel') : t('admin:oauthReauthorize')}</Button>}
      </div>
      {reauthorizing && <OAuthLoginCard provider={provider} name="" models={[]} channelId={channelId} channelKeyId={row.id}
        onDone={() => { setReauthorizing(false); onDone() }} />}
    </div>
  )
}
