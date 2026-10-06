import { useTranslation } from 'react-i18next'

export function useQuotaLabels() {
  const { t, i18n } = useTranslation()
  const windowLabel = (seconds: number | null) => {
    if (seconds === null || seconds <= 0) return t('admin:channelQuotaWindowUnknown')
    const [divisor, unit] = seconds >= 86400 ? [86400, 'day'] : seconds >= 3600 ? [3600, 'hour'] : seconds >= 60 ? [60, 'minute'] : [1, 'second']
    return new Intl.NumberFormat(i18n.language, { style: 'unit', unit: unit as string, maximumFractionDigits: 2 }).format(seconds / Number(divisor))
  }
  const quotaLabel = (seconds: number) => seconds === 604800 ? t('admin:channelQuotaWeeklyLimit')
    : t('admin:channelQuotaSessionLimit', { window: windowLabel(seconds) })
  return { windowLabel, quotaLabel }
}
