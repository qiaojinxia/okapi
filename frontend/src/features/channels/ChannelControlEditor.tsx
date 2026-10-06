import type { ReactNode } from 'react'
import { useTranslation } from 'react-i18next'
import type { AccountControl } from './types'
import { useAccountCapabilities } from './account-controls/api'
import { useControlDraft } from './account-controls/use-control-draft'
import { CommonControls } from './account-controls/CommonControls'
import { SubscriptionControls } from './account-controls/SubscriptionControls'

export function ChannelControlEditor({ value, onChange, onValidChange, provider, refreshable, channelId,
  summaryPrefix, concurrencyValid = true, children }: {
  value: AccountControl | undefined
  onChange: (value: AccountControl) => void
  onValidChange: (valid: boolean) => void
  provider: string
  refreshable: boolean
  channelId?: number
  summaryPrefix?: string
  concurrencyValid?: boolean
  children?: ReactNode
}) {
  const { t } = useTranslation()
  const providers = useAccountCapabilities(provider)
  const draft = useControlDraft(value, providers.capabilities, refreshable, onChange, onValidChange)
  const { legacyUsage } = draft
  return <>
    <CommonControls draft={draft} summaryPrefix={summaryPrefix} concurrencyValid={concurrencyValid}>{children}</CommonControls>
    {legacyUsage && [legacyUsage.requests, legacyUsage.tokens, legacyUsage.cost_micro].some((limit) => limit != null)
      && <p role="note" className="my-2 text-xs leading-5 text-muted-foreground">{t('admin:channelLegacyLimitsRetired')}</p>}
    {providers.capabilities?.subscription && <SubscriptionControls capabilities={providers.capabilities}
      draft={draft} refreshable={refreshable} channelId={channelId} />}
  </>
}
