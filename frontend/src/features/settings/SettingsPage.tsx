import { Bell, Megaphone, Settings, SlidersHorizontal, UserPlus } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { NoticeCard } from '@/features/settings/NoticeCard'
import { NotifyCard } from '@/features/settings/NotifyCard'
import { PageHeader } from '@/components/ui/page'
import { RegistrationCard } from '@/features/settings/RegistrationCard'
import { SettingsCard } from '@/features/settings/SettingsKeyValues'
import { Tabs } from '@/components/ui/tabs'

const SETTINGS_TABS = ['values', 'registration', 'notice', 'notify'] as const
type SettingsTab = (typeof SETTINGS_TABS)[number]

/// 系统设置页：只放配置，不放会改动既有数据的动作（退款、留存清理在 /admin/ops）。
/// 全量设置表本身就很长，公告与通知多路配置又各是一整块表单——页签分开，
/// 免得找一个键要滚过整片表单。
export function SettingsPage() {
  const { t } = useTranslation()
  const [tab, setTab] = useState<SettingsTab>('values')

  const items = [
    { id: 'values', label: t('admin:settingKeyValues'), icon: SlidersHorizontal },
    { id: 'registration', label: t('admin:regTitle'), icon: UserPlus },
    { id: 'notice', label: t('admin:noticeTitle'), icon: Megaphone },
    { id: 'notify', label: t('admin:notify'), icon: Bell },
  ]

  return (
    <div className="flex flex-col gap-4">
      <PageHeader title={t('admin:settingsTitle')} description={t('admin:settingsHint')} icon={Settings} />
      <Tabs
        variant="underline"
        items={items}
        active={tab}
        onChange={(id) => setTab(id as SettingsTab)}
      />
      {tab === 'values' && <SettingsCard />}
      {tab === 'registration' && <RegistrationCard />}
      {tab === 'notice' && <NoticeCard />}
      {tab === 'notify' && <NotifyCard />}
    </div>
  )
}
