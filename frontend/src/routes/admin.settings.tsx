import { createFileRoute } from '@tanstack/react-router'
import { SettingsPage } from '@/features/settings/SettingsPage'
import { SETTINGS_TABS } from '@/features/settings/setting-catalog'
import type { SettingsTab } from '@/features/settings/setting-catalog'
import { oneOf } from '@/lib/search-params'

/// 页签在 URL：别的页可以 `<Link search={{ tab: 'egress' }}>` 直达某个页签（出口代理页就这么跳过来）。
export const Route = createFileRoute('/admin/settings')({
  validateSearch: (search: Record<string, unknown>): { tab?: SettingsTab } => ({ tab: oneOf(search.tab, SETTINGS_TABS) }),
  component: SettingsPage,
})
