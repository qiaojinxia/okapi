import { LockKeyhole } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { Input, Label } from '@/components/ui/input'
import type { pricingDisabledReason } from './model-pricing-availability'

export function ModelRateField({ id, label, value, onChange, placeholder, disabledReason }: {
  id: string
  label: string
  value: string
  onChange: (value: string) => void
  placeholder?: string
  disabledReason: ReturnType<typeof pricingDisabledReason>
}) {
  const { t } = useTranslation()
  return <div className="flex min-w-0 flex-col gap-1.5">
    <Label htmlFor={id} className={disabledReason ? 'text-muted-foreground' : undefined}>{label}</Label>
    <Input id={id} value={value} inputMode="decimal" placeholder={placeholder}
      disabled={Boolean(disabledReason)} aria-describedby={disabledReason ? `${id}-reason` : undefined}
      className={disabledReason ? 'border-dashed bg-muted/60 text-muted-foreground disabled:opacity-100' : undefined}
      onChange={(event) => onChange(event.target.value)} />
    {disabledReason && <p id={`${id}-reason`} className="flex items-start gap-1 text-xs leading-5 text-muted-foreground">
      <LockKeyhole aria-hidden="true" className="mt-1 h-3 w-3 shrink-0" />{t(disabledReason)}
    </p>}
  </div>
}
