import { useTranslation } from 'react-i18next'
import { Field } from '@/components/ui/field'
import { Input } from '@/components/ui/input'
import { TagInput } from '@/components/ui/tag-input'
import { Button } from '@/components/ui/button'
import { useCatalog } from '@/features/public-pricing/use-catalog'
import { describeError } from '@/lib/i18n'

export interface KeyLimitsDraft { expires: string; quota: string; models: string[] }
export const emptyKeyLimits = (): KeyLimitsDraft => ({ expires: '', quota: '', models: [] })

export function keyQuotaText(micro: number | null | undefined): string {
  if (micro == null) return ''
  const fraction = String(micro % 1_000_000).padStart(6, '0').replace(/0+$/, '')
  return String(Math.trunc(micro / 1_000_000)) + (fraction ? `.${fraction}` : '')
}

// Exact decimal parsing: do not round an invalid financial limit to zero/unlimited.
export function keyQuotaMicro(text: string): number | null | undefined {
  if (text.trim() === '') return null
  const parts = /^(\d+)(?:\.(\d{1,6}))?$/.exec(text.trim())
  if (!parts) return undefined
  const micro = Number(parts[1] + (parts[2] ?? '').padEnd(6, '0'))
  return Number.isSafeInteger(micro) && micro > 0 ? micro : undefined
}

export function keyLimitsError(draft: KeyLimitsDraft): string | null {
  if (keyQuotaMicro(draft.quota) === undefined) return 'portal:keyQuotaInvalid'
  if (draft.expires && (!Number.isFinite(Date.parse(draft.expires)) || Date.parse(draft.expires) <= Date.now())) return 'portal:keyExpiresInvalid'
  return null
}

export function KeyLimitsFields({ value, onChange, group, quotaSupported }: {
  value: KeyLimitsDraft; onChange: (value: KeyLimitsDraft) => void; group: string; quotaSupported: boolean
}) {
  const { t } = useTranslation()
  const catalog = useCatalog()
  const suggestions = (catalog.data?.models ?? []).filter((m) => group ? m.groups.includes(group) : m.groups.length > 0).map((m) => ({ value: m.model, label: m.display_name ?? undefined }))
  return <>
    <div className="grid items-start gap-4 sm:grid-cols-2">
      <Field label={t('portal:keyExpires')} htmlFor="key-expires" hint={t('portal:keyExpiresHint', { zone: Intl.DateTimeFormat().resolvedOptions().timeZone })}>
        <Input id="key-expires" type="datetime-local" step={1} value={value.expires} onChange={(e) => onChange({ ...value, expires: e.target.value })} />
        {value.expires && <Button variant="ghost" size="sm" className="self-start" onClick={() => onChange({ ...value, expires: '' })}>{t('portal:keyNoExpiry')}</Button>}
      </Field>
      <Field label={t('portal:keyQuota')} htmlFor="key-quota" hint={t(quotaSupported ? 'portal:keyQuotaHint' : 'portal:keyLimitsUpgrade')}>
        <Input id="key-quota" inputMode="decimal" disabled={!quotaSupported} value={value.quota} placeholder={t('portal:keyUnlimited')} onChange={(e) => onChange({ ...value, quota: e.target.value })} />
      </Field>
    </div>
    <Field label={t('portal:keyModels')} htmlFor="key-models" hint={t('portal:keyModelsHint')}>
      <TagInput id="key-models" value={value.models} onChange={(models) => onChange({ ...value, models })} suggestions={suggestions} placeholder={t('portal:keyModelsPlaceholder')} />
      {catalog.isError && <p className="text-xs text-destructive">{describeError(catalog.error)} <button type="button" className="underline" onClick={() => void catalog.refetch()}>{t('common:retry')}</button></p>}
    </Field>
  </>
}
