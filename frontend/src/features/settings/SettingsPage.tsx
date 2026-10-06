import { getRouteApi } from '@tanstack/react-router'
import { Bell, Bot, Globe, Mail, Megaphone, Settings, ShieldCheck, SlidersHorizontal, UserPlus } from 'lucide-react'
import { useEffect, useRef } from 'react'
import { useTranslation } from 'react-i18next'
import { EgressSettings } from '@/features/settings/EgressSettings'
import { NoticeCard } from '@/features/settings/NoticeCard'
import { NotifyCard } from '@/features/settings/NotifyCard'
import { PrivacyCard } from '@/features/settings/PrivacyCard'
import { PageHeader } from '@/components/ui/page'
import { RegistrationCard } from '@/features/settings/RegistrationCard'
import { SettingsCard } from '@/features/settings/SettingsKeyValues'
import { SmtpCard } from '@/features/settings/SmtpCard'
import { McpSettings } from '@/features/settings/McpSettings'
import { TabPanel, Tabs } from '@/components/ui/tabs'
import { SETTINGS_TABS } from '@/features/settings/setting-catalog'
import type { SettingsSection, SettingsTab } from '@/features/settings/setting-catalog'

const routeApi = getRouteApi('/admin/settings')

/// 系统设置页：只放配置，不放会改动既有数据的动作（退款、留存清理在 /admin/ops）。
/// 常用表单在前，全量键值配置收在高级设置。访问过的面板保留草稿，切签不丢输入。
/// 当前页签在 URL（`?tab=`，缺省为注册与风控），别的页可以直达某个页签。
export function SettingsPage() {
  const { t } = useTranslation()
  const search = routeApi.useSearch(), navigate = routeApi.useNavigate()
  const tab: SettingsTab = search.tab ?? 'registration'
  const setTab = (next: SettingsTab) => {
    void navigate({ search: next === 'registration' ? {} : { tab: next } })
  }
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
    { id: 'smtp', label: t('admin:smtpTitle'), icon: Mail },
    { id: 'privacy', label: t('admin:privacyTitle'), icon: ShieldCheck },
    { id: 'ai', label: t('admin:mcpTitle'), icon: Bot },
    { id: 'egress', label: t('admin:proxiesTitle'), icon: Globe },
    { id: 'values', label: t('admin:settingAdvanced'), icon: SlidersHorizontal },
  ]
  const panels = {
    registration: <RegistrationCard />,
    notice: <NoticeCard />,
    notify: <NotifyCard />,
    smtp: <SmtpCard />,
    privacy: <PrivacyCard />,
    ai: <McpSettings />,
    egress: <EgressSettings />,
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
