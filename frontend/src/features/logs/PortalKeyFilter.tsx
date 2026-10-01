import { useQuery } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { AutocompleteInput } from '@/components/ui/autocomplete-input'
import { apiFetch } from '@/lib/api'

export function PortalKeyFilter({ value, onChange, onChoose, onSubmit, className = 'w-full sm:w-80' }: { value: string; onChange: (value: string) => void; onChoose: (value: string) => void; onSubmit: () => void; className?: string }) {
  const { t } = useTranslation()
  const [focused, setFocused] = useState(false)
  const keys = useQuery({
    queryKey: ['portal-log-keys'],
    queryFn: () => apiFetch<{ data: { id: number; name: string; key_prefix: string }[] }>('/api/me/keys'),
    enabled: focused, staleTime: 60_000, retry: false,
  })
  const chosen = keys.data?.data.find((key) => String(key.id) === value)
  return <AutocompleteInput className={className} value={value} onChange={onChange} onChoose={onChoose} onSubmit={onSubmit}
    aria-label={t('logs:keyFilter')} placeholder={t('logs:keyFilterHint')}
    displayValue={!focused ? chosen?.name : undefined} optionLabelFirst search
    options={(keys.data?.data ?? []).map((key) => ({ value: String(key.id), label: key.name || `#${key.id}`, description: `#${key.id} · ${key.key_prefix}…` }))}
    loading={keys.isFetching} error={keys.isError ? t('logs:keyLookupError') : undefined} emptyHint={t('logs:keyFilterHint')}
    onFocus={() => setFocused(true)} onBlur={() => setFocused(false)} />
}
