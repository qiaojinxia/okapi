import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { OptionalSection } from '@/components/ui/optional-section'
import { Input, Label } from '@/components/ui/input'
import type { AccountCapabilities } from './api'
import { useAccountUsage } from './api'
import type { ControlDraft } from './use-control-draft'
import { useQuotaLabels } from './use-quota-labels'
import { LocalTokenControls } from './LocalTokenControls'
import { QuotaObservation } from './QuotaObservation'
import { RenewalControls } from './RenewalControls'

export function SubscriptionControls({ capabilities, draft, refreshable, channelId }: {
  capabilities: AccountCapabilities; draft: ControlDraft; refreshable: boolean; channelId?: number
}) {
  const { t } = useTranslation()
  const { quotaLabel } = useQuotaLabels()
  const { policy, quotaWindows, quotaText, quotaValid, changeQuota, tokenDraft, tokenPeriod, tokenValid, refreshValid } = draft
  const [open, setOpen] = useState(false)
  const usage = useAccountUsage(channelId, tokenPeriod, open)
  const subscriptionError = !quotaValid ? t('admin:channelQuotaInvalid') : !tokenValid ? t('admin:channelLocalTokenInvalid') : !refreshValid ? t('admin:channelControlNumberInvalid') : undefined
  const renewalSummary = capabilities.refresh ? t(!refreshable ? 'admin:channelTokenOnlySummary'
    : policy.refresh_mode === 'external' ? 'admin:channelRefreshOff' : 'admin:channelRefreshOn') : undefined
  const summary = [...quotaWindows.map((seconds) => quotaText(seconds) && `${quotaLabel(seconds)}: ${quotaText(seconds)}%`),
    tokenDraft && t('admin:channelLocalTokenSummary', { count: tokenDraft }), renewalSummary].filter(Boolean).join(' · ') || t('admin:channelSubscriptionDefault')
  return <OptionalSection id="channel-subscription-controls" title={t('admin:channelSubscriptionControls')}
    summary={summary} hint={t('admin:channelSubscriptionControlsHint')} error={subscriptionError} onOpenChange={setOpen}>
      {capabilities?.quota && <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">{quotaWindows.map((seconds) => <div key={seconds} className="flex min-w-0 flex-col gap-1.5">
        <Label htmlFor={`channel-quota-${seconds}`}>{quotaLabel(seconds)}</Label>
        <Input id={`channel-quota-${seconds}`} inputMode="numeric" value={quotaText(seconds)} placeholder={t('admin:channelLimitUnlimited')} aria-invalid={!quotaValid}
          onChange={(e) => {
            changeQuota(seconds, e.target.value)
          }} />
      </div>)}<p className="text-xs leading-5 text-muted-foreground sm:col-span-2">{t('admin:channelQuotaIndependentHint')}</p></div>}
    <LocalTokenControls draft={draft} usage={usage.data} />
    {channelId !== undefined && capabilities.quota && <QuotaObservation usage={usage} policy={policy} />}
    {capabilities.refresh && <RenewalControls draft={draft} refreshable={refreshable} />}
  </OptionalSection>
}
