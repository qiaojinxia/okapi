import { useTranslation } from 'react-i18next'
import { Segmented } from '@/components/ui/segmented'
import type { Scope } from '@/features/portal-overview/types'

export function UsageScope({ scope, onChange, accountLogin, keyLabel, ariaLabel }: {
  scope: Scope; onChange: (value: Scope) => void; accountLogin: boolean; keyLabel: string; ariaLabel?: string
}) {
  const { t } = useTranslation()
  return <div className="flex min-w-0 max-w-full flex-wrap items-center gap-2">
    <span className="text-xs text-muted-foreground">{t('portal:logsScope')}</span>
    {accountLogin ? <span className="text-sm font-medium">{t('portal:accountScope')}</span> : <>
      <Segmented ariaLabel={ariaLabel ?? t('portal:logsScope')} value={scope} onChange={onChange} options={[
        { value: 'key', label: t('portal:scopeKey') }, { value: 'user', label: t('portal:scopeUser') },
      ]} />
      {scope === 'key' && <span className="max-w-64 truncate text-xs text-muted-foreground" title={keyLabel}>{keyLabel}</span>}
    </>}
  </div>
}
