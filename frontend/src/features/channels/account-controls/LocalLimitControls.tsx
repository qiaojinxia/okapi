import { useTranslation } from 'react-i18next'
import { Input, Label } from '@/components/ui/input'
import { Select } from '@/components/ui/select'
import { formatMoney } from '@/lib/money'
import type { LocalPeriod } from '../types'
import type { UsageResponse } from './api'
import { LOCAL_PERIODS } from './policy'
import type { ControlDraft } from './use-control-draft'

/// 按本站账单算的两项上限，任何渠道都能设：成本（美元）与 Token，各带自己的周期。
/// 已有的渠道在输入框下给出本周期已记录的用量，设上限时有个参照。
export function LocalLimitControls({ draft, usage }: { draft: ControlDraft; usage?: UsageResponse }) {
  const { t, i18n } = useTranslation()
  const { costDraft, costPeriod, costValid, changeCost, changeCostPeriod,
    tokenDraft, tokenPeriod, tokenValid, changeTokens, changeTokenPeriod } = draft
  const periods = LOCAL_PERIODS.map((period) => ({ value: period, label: t(`admin:channelPeriod_${period}`) }))
  // 响应里缺字段（旧服务端、接口桩）就不显示用量，不影响填写
  const cost = usage?.cost_usage?.cost_micro, tokens = usage?.token_usage?.tokens
  return (
    <div data-slot="channel-local-limits" className="flex flex-col gap-3">
      <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
        <div className="flex min-w-0 flex-col gap-1.5">
          <Label htmlFor="channel-limit-cost">{t('admin:channelLimitCost')}</Label>
          <Input id="channel-limit-cost" inputMode="decimal" value={costDraft} placeholder={t('admin:channelLimitUnlimited')}
            aria-invalid={!costValid} onChange={(e) => changeCost(e.target.value)} />
          {typeof cost === 'number' && <p data-slot="channel-cost-used" className="text-xs text-muted-foreground">
            {t('admin:channelLocalCostUsed', { amount: formatMoney(cost, i18n.language) })}
          </p>}
        </div>
        <div className="flex min-w-0 flex-col gap-1.5">
          <Label htmlFor="channel-cost-period">{t('admin:channelCostPeriod')}</Label>
          <Select id="channel-cost-period" className="w-full" value={costPeriod} options={periods}
            onChange={(period) => changeCostPeriod(period as LocalPeriod)} />
        </div>
        <div className="flex min-w-0 flex-col gap-1.5">
          <Label htmlFor="channel-limit-tokens">{t('admin:channelLimit_tokens')}</Label>
          <Input id="channel-limit-tokens" inputMode="numeric" value={tokenDraft} placeholder={t('admin:channelLimitUnlimited')}
            aria-invalid={!tokenValid} onChange={(e) => changeTokens(e.target.value)} />
          {typeof tokens === 'number' && <p data-slot="channel-tokens-used" className="text-xs text-muted-foreground">
            {t('admin:channelLocalTokenUsed', { count: tokens.toLocaleString(i18n.language) })}
          </p>}
        </div>
        <div className="flex min-w-0 flex-col gap-1.5">
          <Label htmlFor="channel-token-period">{t('admin:channelTokenPeriod')}</Label>
          <Select id="channel-token-period" className="w-full" value={tokenPeriod} options={periods}
            onChange={(period) => changeTokenPeriod(period as LocalPeriod)} />
        </div>
      </div>
      <p className="text-xs leading-5 text-muted-foreground">{t('admin:channelLocalLimitsHint')}</p>
    </div>
  )
}
