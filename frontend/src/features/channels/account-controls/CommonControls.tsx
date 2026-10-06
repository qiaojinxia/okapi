import type { ReactNode } from 'react'
import { useTranslation } from 'react-i18next'
import { OptionalSection } from '@/components/ui/optional-section'
import { Input, Label } from '@/components/ui/input'
import { commonNumbers } from './policy'
import type { ControlDraft } from './use-control-draft'

export function CommonControls({ draft, summaryPrefix, concurrencyValid, children }: {
  draft: ControlDraft; summaryPrefix?: string; concurrencyValid: boolean; children?: ReactNode
}) {
  const { t } = useTranslation()
  const { numbers, commonChanged, commonValid, validNumber, changeNumber } = draft
  const commonSummary = [summaryPrefix, commonChanged && t('admin:channelCommonSummary', {
    rate: numbers.rate_limit_cooldown_secs, failures: numbers.failure_threshold, pause: numbers.failure_cooldown_secs,
  })].filter(Boolean).join(' · ') || t('admin:channelOptionsDefault')
  const commonError = !concurrencyValid ? t('admin:channelConcurrencyInvalid') : !commonValid ? t('admin:channelControlNumberInvalid') : undefined
  return (
    <OptionalSection id="channel-limits" title={t('admin:channelCommonControls')} hint={t('admin:channelCommonControlsHint')} summary={commonSummary} error={commonError}>
      <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
        {children}
        {commonNumbers.map(([name, label, min, max]) => <div key={name} className="flex min-w-0 flex-col gap-1.5">
          <Label htmlFor={`channel-${name}`}>{t(`admin:${label}`)}</Label>
          <Input id={`channel-${name}`} inputMode="numeric" value={numbers[name]} aria-invalid={!validNumber(name, min, max)}
            onChange={(e) => changeNumber(name, e.target.value, min, max)} />
          <p className="text-xs text-muted-foreground">{t('admin:channelControlNumberRange', { min, max })}</p>
        </div>)}
      </div>
    </OptionalSection>
  )
}
