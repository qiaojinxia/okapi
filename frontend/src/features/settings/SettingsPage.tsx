import { useTranslation } from 'react-i18next'
import { NotifyCard } from '@/features/settings/NotifyCard'
import { PageHeader } from '@/components/ui/page'
import { SettingsCard } from '@/features/settings/SettingsKeyValues'

/// 系统设置页：只放配置，不放会改动既有数据的动作（退款、留存清理在 /admin/ops）。
export function SettingsPage() {
  const { t } = useTranslation()
  return (
    <div className="flex flex-col gap-4">
      <PageHeader title={t('admin:settingsTitle')} description={t('admin:settingsHint')} />
      <NotifyCard />
      <SettingsCard />
    </div>
  )
}
