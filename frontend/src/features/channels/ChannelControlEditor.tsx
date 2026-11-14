import type { ReactNode } from 'react'
import { useTranslation } from 'react-i18next'
import type { AccountControl } from './types'
import { useAccountCapabilities } from './account-controls/api'
import { useControlDraft } from './account-controls/use-control-draft'
import { CommonControls } from './account-controls/CommonControls'
import { QuotaLimitControls } from './account-controls/QuotaLimitControls'
import { RenewalControls } from './account-controls/RenewalControls'

/// 渠道的账号级控制，三段：额度上限（所有渠道）、限流与失败后的暂停（所有渠道）、订阅授权的续期（订阅渠道）。
export function ChannelControlEditor({ value, onChange, onValidChange, provider, refreshable, channelId,
  summaryPrefix, concurrencyValid = true, limitsOpen, children }: {
  value: AccountControl | undefined
  onChange: (value: AccountControl) => void
  onValidChange: (valid: boolean) => void
  provider: string
  refreshable: boolean
  channelId?: number
  summaryPrefix?: string
  concurrencyValid?: boolean
  limitsOpen?: boolean
  children?: ReactNode
}) {
  const { t } = useTranslation()
  const providers = useAccountCapabilities(provider)
  const draft = useControlDraft(value, providers.capabilities, refreshable, onChange, onValidChange)
  const { legacyUsage } = draft
  return <>
    <QuotaLimitControls capabilities={providers.capabilities} draft={draft} channelId={channelId} defaultOpen={limitsOpen} />
    {legacyUsage && [legacyUsage.requests, legacyUsage.tokens, legacyUsage.cost_micro].some((limit) => limit != null)
      && <p role="note" className="my-2 text-xs leading-5 text-muted-foreground">{t('admin:channelLegacyLimitsRetired')}</p>}
    <CommonControls draft={draft} summaryPrefix={summaryPrefix} concurrencyValid={concurrencyValid}>{children}</CommonControls>
    {providers.capabilities?.subscription && providers.capabilities.refresh
      && <RenewalControls draft={draft} refreshable={refreshable} />}
  </>
}
