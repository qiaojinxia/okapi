import { useTranslation } from 'react-i18next'
import { OptionalSection } from '@/components/ui/optional-section'
import { Input, Label } from '@/components/ui/input'
import { Switch } from '@/components/ui/switch'
import type { ControlDraft } from './use-control-draft'

export function RenewalControls({ draft, refreshable }: { draft: ControlDraft; refreshable: boolean }) {
  const { t } = useTranslation()
  const { policy, numbers, refreshValid, changeNumber, setRenewal } = draft
  const summary = t(!refreshable ? 'admin:channelTokenOnlySummary'
    : policy.refresh_mode === 'external' ? 'admin:channelRefreshOff' : 'admin:channelRefreshOn')
  return (
    <OptionalSection id="channel-authorization-options" title={t('admin:channelAuthorizationTitle')} summary={summary}>
      {refreshable ? <>
        <Switch label={t('admin:channelRefreshEnabled')} checked={policy.refresh_mode === 'managed'}
          description={t(policy.refresh_mode === 'external' ? 'admin:channelRefreshExternalHint' : 'admin:channelRefreshManagedHint')}
          onChange={setRenewal} />
        {policy.refresh_mode === 'managed' && (
          <OptionalSection id="channel-renewal-advanced" title={t('admin:channelRefreshAdvanced')}
            summary={t('admin:channelRefreshMarginSummary', { seconds: numbers.refresh_margin_secs })}
            error={!refreshValid ? t('admin:channelControlNumberInvalid') : undefined}>
            <div className="flex flex-col gap-1.5">
              <Label htmlFor="channel-refresh-margin">{t('admin:channelRefreshMargin')}</Label>
              <Input id="channel-refresh-margin" inputMode="numeric" value={numbers.refresh_margin_secs}
                aria-invalid={!refreshValid} onChange={(e) => changeNumber('refresh_margin_secs', e.target.value, 120, 3600)} />
              <p className="text-xs text-muted-foreground">{t('admin:channelControlNumberRange', { min: 120, max: 3600 })}</p>
            </div>
          </OptionalSection>
        )}
      </> : <p className="text-xs leading-5 text-muted-foreground">{t('admin:channelRefreshTokenOnlyHint')}</p>}
    </OptionalSection>
  )
}
