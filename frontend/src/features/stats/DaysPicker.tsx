import { useTranslation } from 'react-i18next'
import { Segmented } from '@/components/ui/segmented'

/// 时间窗选择（1 / 7 / 30 天）：分段选择器形态，与门户看板、分析页同款。
export function DaysPicker({ days, onPick }: { days: number; onPick: (d: number) => void }) {
  const { t } = useTranslation()
  return (
    <Segmented
      value={days}
      onChange={onPick}
      options={[1, 7, 30].map((d) => ({ value: d, label: t('admin:lastDays', { days: d }) }))}
    />
  )
}
