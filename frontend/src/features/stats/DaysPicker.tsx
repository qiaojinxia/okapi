import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'

export function DaysPicker({ days, onPick }: { days: number; onPick: (d: number) => void }) {
  const { t } = useTranslation()
  return (
    <div className="flex gap-2">
      {[1, 7, 30].map((d) => (
        <Button
          key={d}
          size="sm"
          variant={days === d ? 'default' : 'outline'}
          onClick={() => onPick(d)}
        >
          {t('admin:lastDays', { days: d })}
        </Button>
      ))}
    </div>
  )
}
