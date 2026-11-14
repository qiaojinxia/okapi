import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { OptionalSection } from '@/components/ui/optional-section'
import { Input, Label } from '@/components/ui/input'
import type { AccountCapabilities } from './api'
import { useAccountUsage } from './api'
import type { ControlDraft } from './use-control-draft'
import { useQuotaLabels } from './use-quota-labels'
import { LocalLimitControls } from './LocalLimitControls'
import { QuotaObservation } from './QuotaObservation'

/// 「额度上限」：一条渠道最多用到哪儿，都在这一段里设。
/// 任何渠道都有按本站账单算的成本 / Token 上限；订阅渠道另有上游各窗口的百分比上限（窗口与用量来自上游）。
/// 折起时不查用量；展开后才查，已有渠道据此显示各窗口的用量条和本周期已记录的成本 / Token。
export function QuotaLimitControls({ capabilities, draft, channelId, defaultOpen = false }: {
  capabilities: AccountCapabilities | undefined; draft: ControlDraft; channelId?: number; defaultOpen?: boolean
}) {
  const { t } = useTranslation()
  const { quotaLabel, windowLabel } = useQuotaLabels()
  const { policy, quotaWindows, quotaText, quotaValid, changeQuota, costDraft, costPeriod, costValid, tokenDraft, tokenPeriod, tokenValid } = draft
  const [open, setOpen] = useState(defaultOpen)
  const usage = useAccountUsage(channelId, { tokens: tokenPeriod, cost: costPeriod }, open)
  const upstream = Boolean(capabilities?.subscription && capabilities.quota)
  const error = !quotaValid ? t('admin:channelQuotaInvalid') : !costValid ? t('admin:channelLocalCostInvalid')
    : !tokenValid ? t('admin:channelLocalTokenInvalid') : undefined
  const summary = [
    ...(upstream ? quotaWindows.map((seconds) => quotaText(seconds)
      && t('admin:channelQuotaWindowSummary', { window: windowLabel(seconds), percent: quotaText(seconds) })) : []),
    costDraft && t('admin:channelLocalCostSummary', { amount: costDraft, period: t(`admin:channelPeriodShort_${costPeriod}`) }),
    tokenDraft && t('admin:channelLocalTokenSummary', { count: tokenDraft, period: t(`admin:channelPeriodShort_${tokenPeriod}`) }),
  ].filter(Boolean).join(' · ') || t(upstream ? 'admin:channelSubscriptionDefault' : 'admin:channelLimitsDefault')
  return <OptionalSection id="channel-quota-limits" title={t('admin:channelLimitsTitle')}
    summary={summary} hint={t('admin:channelLimitsHint')} error={error} defaultOpen={defaultOpen} onOpenChange={setOpen}>
    {upstream && <div data-slot="channel-upstream-limits" className="flex flex-col gap-3">
      <h4 className="text-xs font-medium text-foreground">{t('admin:channelUpstreamLimits')}</h4>
      <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">{quotaWindows.map((seconds) => <div key={seconds} className="flex min-w-0 flex-col gap-1.5">
        <Label htmlFor={`channel-quota-${seconds}`}>{quotaLabel(seconds)}</Label>
        <Input id={`channel-quota-${seconds}`} inputMode="numeric" value={quotaText(seconds)} placeholder={t('admin:channelLimitUnlimited')} aria-invalid={!quotaValid}
          onChange={(e) => changeQuota(seconds, e.target.value)} />
      </div>)}<p className="text-xs leading-5 text-muted-foreground sm:col-span-2">{t('admin:channelQuotaIndependentHint')}</p></div>
      {channelId !== undefined && <QuotaObservation usage={usage} policy={policy} />}
    </div>}
    {upstream && <h4 className="text-xs font-medium text-foreground">{t('admin:channelLocalLimits')}</h4>}
    <LocalLimitControls draft={draft} usage={usage.data} />
  </OptionalSection>
}
