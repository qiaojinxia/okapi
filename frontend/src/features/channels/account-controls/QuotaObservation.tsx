import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { describeError } from '@/lib/i18n'
import type { AccountControl } from '../types'
import { useAccountUsage } from './api'
import { QuotaMeters } from './QuotaMeters'

/// 渠道编辑里的订阅额度：每把 key 一组用量条，配置了百分比上限的窗口画出上限线。
export function QuotaObservation({ usage, policy }: { usage: ReturnType<typeof useAccountUsage>; policy: AccountControl }) {
  const { t, i18n } = useTranslation()
  if (usage.isError) {
    return (
      <div className="flex items-center justify-between gap-2 rounded-lg border border-border p-3 text-xs">
        <p role="alert">{describeError(usage.error)}</p>
        <Button size="sm" variant="outline" disabled={usage.isFetching} onClick={() => void usage.refetch()}>{t('common:retry')}</Button>
      </div>
    )
  }
  if (!usage.data) {
    return <p className="rounded-lg border border-border p-3 text-xs text-muted-foreground">{t('common:loading')}</p>
  }
  const { quotas, timezone } = usage.data
  const observed = (at: number) => new Date(at * 1000).toLocaleTimeString(i18n.language, { timeZone: timezone, timeStyle: 'short' })
  return (
    <div className="flex flex-col gap-3 rounded-lg border border-border p-3">
      {quotas.length === 0 && <p className="text-xs text-muted-foreground">{t('admin:channelQuotaUnknown')}</p>}
      {quotas.map(({ key_id, quota }) => (
        <div key={key_id} className="flex flex-col gap-2">
          {quotas.length > 1 && <p className="text-xs font-medium">#{key_id}</p>}
          {quota ? <>
            <QuotaMeters quota={quota} limits={policy.quota_limits} />
            <p className="text-[11px] text-muted-foreground">
              {t('admin:channelQuotaObservedAt', { at: observed(quota.observed_at) })}
              {policy.quota_aware && quota.threshold_window && <> · {t('admin:channelQuotaThresholdWindow')}</>}
            </p>
          </> : <p className="text-xs text-muted-foreground">{t('admin:channelQuotaUnknown')}</p>}
        </div>
      ))}
    </div>
  )
}
