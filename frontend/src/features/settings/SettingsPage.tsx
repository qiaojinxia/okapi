import { Bell, Megaphone, Settings, ShieldCheck, SlidersHorizontal, UserPlus } from 'lucide-react'
import { useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { NoticeCard } from '@/features/settings/NoticeCard'
import { NotifyCard } from '@/features/settings/NotifyCard'
import { PrivacyCard } from '@/features/settings/PrivacyCard'
import { PageHeader } from '@/components/ui/page'
import { RegistrationCard } from '@/features/settings/RegistrationCard'
import { SettingsCard } from '@/features/settings/SettingsKeyValues'
import { TabPanel, Tabs } from '@/components/ui/tabs'
import type { SettingsSection } from '@/features/settings/setting-catalog'

const SETTINGS_TABS = ['registration', 'notice', 'notify', 'privacy', 'values'] as const
type SettingsTab = (typeof SETTINGS_TABS)[number]

/// 系统设置页：只放配置，不放会改动既有数据的动作（退款、留存清理在 /admin/ops）。
/// 常用表单在前，全量键值配置收在高级设置。访问过的面板保留草稿，切签不丢输入。
export function SettingsPage() {
  const { t } = useTranslation()
  const [tab, setTab] = useState<SettingsTab>('registration')
  const focusSection = useRef<SettingsSection | null>(null)
  const openSection = (section: SettingsSection) => {
    focusSection.current = section
    setTab(section)
  }
  useEffect(() => {
    if (focusSection.current !== tab) return
    focusSection.current = null
    document.getElementById(`settings-tabs-${tab}`)?.focus({ preventScroll: true })
    document.getElementById('settings-tabs')?.scrollIntoView({ block: 'start' })
  }, [tab])

  const items = [
    { id: 'registration', label: t('admin:regTitle'), icon: UserPlus },
    { id: 'notice', label: t('admin:noticeTitle'), icon: Megaphone },
    { id: 'notify', label: t('admin:notify'), icon: Bell },
    { id: 'privacy', label: t('admin:privacyTitle'), icon: ShieldCheck },
    { id: 'values', label: t('admin:settingAdvanced'), icon: SlidersHorizontal },
  ]
  const panels = {
    registration: <RegistrationCard />,
    notice: <NoticeCard />,
    notify: <NotifyCard />,
    privacy: <PrivacyCard />,
    values: <SettingsCard onOpenSection={openSection} />,
  }

  return (
    <div className="flex flex-col gap-4">
      <PageHeader title={t('admin:settingsTitle')} description={t('admin:settingsHint')} icon={Settings} />
      <Tabs
        id="settings-tabs"
        className="scroll-mt-20"
        ariaLabel={t('admin:settingsTitle')}
        variant="underline"
        items={items.map((item) => ({ ...item, panelId: `settings-panel-${item.id}` }))}
        active={tab}
        onChange={(id) => setTab(id as SettingsTab)}
      />
      {SETTINGS_TABS.map((id) => (
        <TabPanel key={id} id={`settings-panel-${id}`} labelledBy={`settings-tabs-${id}`} active={tab === id}>
          {panels[id]}
        </TabPanel>
      ))}
    </div>
  )
}
