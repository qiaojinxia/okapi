import { useTranslation } from 'react-i18next'
import type { ChannelRow } from '@/features/channels/types'
import { FieldGroup } from '@/components/ui/drawer'
import { KeyParamRow } from '@/features/channels/KeyParamRow'
import { OAuthKeyHealth } from '@/features/channels/oauth-key-health'

/// 每把 key 的并发与权重属于调度；凭证状态（到期、刷新、重新授权）由 `CredentialStatus` 放在接入信息。
export function KeySchedule({ channel, onDone }: { channel: ChannelRow; onDone: () => void }) {
  const { t } = useTranslation()
  const keys = channel.keys ?? []
  const refreshEnabled = channel.settings?.account_control?.refresh_mode !== 'external'
  const rows = keys.map((k) => (
    <KeyParamRow key={k.id} channelId={channel.id} provider={channel.provider} row={k}
      showIdentity={keys.length > 1} showCredential={false} refreshEnabled={refreshEnabled} onDone={onDone} />
  ))
  return (
    <section data-slot="channel-key-schedule" className="border-t border-border py-4 first:border-t-0 first:pt-0 last:pb-0">
      {keys.length <= 1 ? rows
        : <FieldGroup title={t('admin:channelKeys')} hint={t('admin:channelKeysHint')}>{rows}</FieldGroup>}
    </section>
  )
}

/// 订阅凭证的当前状态与它的操作，和上方的凭证输入同属「接入」。
export function CredentialStatus({ channel, onDone }: { channel: ChannelRow; onDone: () => void }) {
  const keys = (channel.keys ?? []).filter((k) => k.credential_kind === 1)
  const refreshEnabled = channel.settings?.account_control?.refresh_mode !== 'external'
  if (keys.length === 0) return null
  return (
    <div data-slot="channel-credential-status" className="flex flex-col gap-3">
      {keys.map((k) => (
        <div key={k.id} className="flex flex-col gap-1">
          {keys.length > 1 && <span className="text-xs font-medium">#{k.id}</span>}
          <OAuthKeyHealth channelId={channel.id} provider={channel.provider} row={k}
            refreshEnabled={refreshEnabled} onDone={onDone} />
        </div>
      ))}
    </div>
  )
}
