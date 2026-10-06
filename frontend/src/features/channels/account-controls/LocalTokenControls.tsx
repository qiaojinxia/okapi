import { useTranslation } from 'react-i18next'
import { OptionalSection } from '@/components/ui/optional-section'
import { Input, Label } from '@/components/ui/input'
import { Select } from '@/components/ui/select'
import type { TokenPeriod, UsageResponse } from './api'
import type { ControlDraft } from './use-control-draft'

export function LocalTokenControls({ draft, usage }: { draft: ControlDraft; usage?: UsageResponse }) {
  const { t, i18n } = useTranslation()
  const { tokenDraft, tokenPeriod, tokenValid, changeTokens, changePeriod } = draft
  return (
      <OptionalSection id="channel-local-token-controls" title={t('admin:channelLocalTokenTitle')}
        summary={tokenDraft ? t('admin:channelLocalTokenSummary', { count: tokenDraft }) : t('admin:channelLimitUnlimited')} error={!tokenValid ? t('admin:channelLocalTokenInvalid') : undefined}>
        <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
          <div className="flex min-w-0 flex-col gap-1.5"><Label htmlFor="channel-limit-tokens">{t('admin:channelLimit_tokens')}</Label>
            <Input id="channel-limit-tokens" inputMode="numeric" value={tokenDraft} placeholder={t('admin:channelLimitUnlimited')} aria-invalid={!tokenValid}
              onChange={(e) => changeTokens(e.target.value)} />
          </div>
          <div className="flex min-w-0 flex-col gap-1.5"><Label htmlFor="channel-token-period">{t('admin:channelUsagePeriod')}</Label>
            <Select id="channel-token-period" className="w-full" value={tokenPeriod}
              onChange={(period) => changePeriod(period as TokenPeriod)}
              options={['total', 'day', 'week'].map((period) => ({ value: period, label: t(`admin:channelPeriod_${period}`) }))} />
          </div>
        </div>
        <p className="text-xs leading-5 text-muted-foreground">{t('admin:channelLocalTokenHint')}</p>
        {usage?.token_usage && <p className="text-xs text-muted-foreground">{t('admin:channelLocalTokenUsed', { count: usage.token_usage.tokens.toLocaleString(i18n.language) })}</p>}
      </OptionalSection>
  )
}
